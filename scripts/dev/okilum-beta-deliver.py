#!/usr/bin/env python3
"""Keep a Linux desktop on the newest Okilum beta without a person (Oleg 14.11).

Run by a systemd --user timer (scripts/dev/systemd/okilum-beta-deliver.*) every
few minutes inside the graphical session. One run:

1. reads the signed Linux beta descriptor and checks it exactly like the in-app
   AppImage updater (crates/okilum-appimage/src/lib.rs): gpgv against the
   pinned release key, the descriptor fields, the payload signature, size and
   sha256, and the signed provenance identity;
2. stops when the managed AppImage (~/.local/share/okilum/Okilum.AppImage)
   already holds that build or a newer one;
3. downloads and verifies the payload in a private cache (no lock held);
4. if Okilum is open, waits until the user has been idle for 60 s (Wayland
   ext-idle-notify-v1, kept open so any later input counts), finds the windows
   again, pins them by pidfd and sends SIGTERM, which the app handles like Quit
   (editors saved, launch marker cleared). Input before the signal cancels the
   run. It never sends SIGKILL: a window still open 30 s later keeps running
   and the next run tries again;
5. checks the installed build again under the locks (never a downgrade), installs with the updater's own transaction beside the AppImage (install and
   update locks, a `.okilum-update-*` stage with previous.AppImage and
   expected.json, the build record, an atomic rename);
6. reopens Okilum if it was open, in its own systemd unit in the app slice,
   with OKILUM_APPIMAGE_UPDATE_RECEIPT so the new build itself confirms the
   start (writes `started` and the build record). If it exits before that,
   the previous AppImage is put back and reopened;
7. appends one line per action to ~/.local/state/okilum-deliver.log.

Never builds anything. Stdlib only. Docs: scripts/dev/README.md.
"""
import argparse
import base64
import contextlib
import datetime
import email.utils
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import socket
import stat as stat_module
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request

ORIGIN = 'https://updates.befeast.com/'
IMAGE = 'Okilum-x86_64.AppImage'
CHANNEL = 'beta'
MAX_IMAGE = 2 * 1024 * 1024 * 1024
OWNER = b'Okilum managed AppImage v1\n'
INSTANCE_LOCK = 'reader-instance.lock'
# Same authority as crates/okilum-appimage (FINGERPRINT and scripts/arch/
# okilum-signing-key.asc); scripts/ci/test_beta_deliver.py keeps them equal.
FINGERPRINT = '7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882'
KEY = '''-----BEGIN PGP PUBLIC KEY BLOCK-----

mDMEasKPJRYJKwYBBAHaRw8BAQdAaKKXan3/inMypleeF78WaKwYgfGMfzfzTFUT
KVcu6Ya0NEJlRmVhc3QgVGVzc2VyYSBBcmNoIFJlbGVhc2VzIDxyZWxlYXNlc0Bi
ZWZlYXN0LmNvbT6IlgQTFgoAPhYhBH/+nyfsyOJT5FuuoIqsv7LJ5HiCBQJqwo8l
AhsDBQkFo5qABQsJCAcCBhUKCQgLAgQWAgMBAh4BAheAAAoJEIqsv7LJ5HiCI/8B
AKNVMZcltPKhMTd74Ae6D5aMaU0RT4NSjIJQJtA1sU3BAQDbh905+PXcTEjUvn5r
ZUqUQByRbLK7GGiGUdXHFUqZAA==
=cUQC
-----END PGP PUBLIC KEY BLOCK-----
'''
RELEASE_FIELDS = ('schema', 'platform', 'channel', 'build', 'version', 'source', 'filename',
                  'sha256', 'size', 'key', 'fingerprint', 'provenance_sha256')
# Fallback to the user's session when the app's own environment is unknown.
RUNTIME_ENV = ('APPIMAGE', 'APPDIR', 'ARGV0', 'OWD', 'LD_LIBRARY_PATH', 'APPIMAGE_EXTRACT_AND_RUN',
               'OKILUM_APPIMAGE_UPDATE_RECEIPT', 'OKILUM_APPRUN_PRELOAD')


class Refused(Exception):
    """Delivery must not continue; nothing was installed."""


# ---------------------------------------------------------------- settings

class Settings:
    def __init__(self, environ=None):
        env = os.environ if environ is None else environ
        home = Path(env.get('HOME') or Path.home())
        state = Path(env['XDG_STATE_HOME']) if env.get('XDG_STATE_HOME', '').startswith('/') else home / '.local/state'
        cache = Path(env['XDG_CACHE_HOME']) if env.get('XDG_CACHE_HOME', '').startswith('/') else home / '.cache'
        self.home = home
        self.origin = env.get('OKILUM_DELIVER_ORIGIN', ORIGIN)
        self.image = Path(env.get('OKILUM_DELIVER_IMAGE') or home / '.local/share/okilum/Okilum.AppImage')
        self.app_state = state / 'okilum'
        self.log = Path(env.get('OKILUM_DELIVER_LOG') or state / 'okilum-deliver.log')
        self.own_state = state / 'okilum-deliver'
        self.cache = cache / 'okilum-deliver'
        self.idle_ms = int(env.get('OKILUM_DELIVER_IDLE_SECONDS', '60')) * 1000
        self.idle_wait = float(env.get('OKILUM_DELIVER_IDLE_WAIT', '1500'))
        self.term_wait = float(env.get('OKILUM_DELIVER_TERM_SECONDS', '30'))
        self.start_wait = float(env.get('OKILUM_DELIVER_START_SECONDS', '90'))
        self.lock_wait = float(env.get('OKILUM_DELIVER_LOCK_SECONDS', '60'))
        self.max_failures = int(env.get('OKILUM_DELIVER_MAX_FAILURES', '2'))
        self.gpgv = env.get('OKILUM_DELIVER_GPGV', 'gpgv')
        self.wayland = wayland_socket(env)


def wayland_socket(env):
    display = env.get('WAYLAND_DISPLAY', '')
    if not display:
        return None
    if display.startswith('/'):
        return display
    runtime = env.get('XDG_RUNTIME_DIR', '')
    return os.path.join(runtime, display) if runtime else None


def now():
    return datetime.datetime.now().astimezone()


def clock(moment):
    return moment.strftime('%H:%M:%S') if moment else '?'


def log_line(settings, text):
    """One action per line, local time with offset (the manager reads it)."""
    line = f'{now().isoformat(timespec="seconds")} {text}'
    print(line, flush=True)
    settings.log.parent.mkdir(parents=True, exist_ok=True)
    with open(settings.log, 'a') as out:
        out.write(line + '\n')


# ------------------------------------------------------------ verification

def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as source:
        for block in iter(lambda: source.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def is_hex(value, length):
    return isinstance(value, str) and len(value) == length and all(c in '0123456789abcdef' for c in value)


def dearmor(armored):
    """Binary keyring from an armored public key, as lib.rs verify_authority does."""
    lines = armored.splitlines()
    body = []
    started = False
    for line in lines:
        if not started:
            started = line == ''
            continue
        if line.startswith('=') or line.startswith('-'):
            break
        body.append(line)
    return base64.b64decode(''.join(body))


def verify_signature(settings, data, signature, key=None, fingerprint=None):
    key, fingerprint = key or KEY, fingerprint or FINGERPRINT
    with tempfile.TemporaryDirectory(prefix='okilum-deliver-gpgv.') as home:
        keyring = Path(home) / 'trusted.gpg'
        keyring.write_bytes(dearmor(key))
        env = {k: v for k, v in os.environ.items() if k != 'GNUPGHOME'}
        result = subprocess.run([settings.gpgv, '--homedir', home, '--keyring', str(keyring), '--status-fd', '1',
                                 str(signature), str(data)], capture_output=True, text=True, env=env)
    if result.returncode != 0:
        raise Refused(f'signature verification failed for {Path(data).name}')
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[0] == '[GNUPG:]' and fields[1] == 'VALIDSIG' and \
                (fields[2] == fingerprint or fields[-1] == fingerprint):
            return
    raise Refused(f'{Path(data).name} is not signed by the pinned Linux release key')


def validate_release(release, fingerprint=None):
    """Release::validate_authority for the beta channel."""
    fingerprint = fingerprint or FINGERPRINT
    try:
        ok = (release['schema'] == 1 and release['platform'] == 'linux-appimage'
              and release['channel'] == CHANNEL
              and isinstance(release['build'], int) and not isinstance(release['build'], bool)
              and release['build'] > 5000 and release['version'] == f"0.1.{release['build']}"
              and is_hex(release['source'], 40) and is_hex(release['sha256'], 64)
              and is_hex(release['provenance_sha256'], 64)
              and release['filename'] == IMAGE
              and release['key'] == f"okilum/linux/builds/{release['build']}/{IMAGE}"
              and release['fingerprint'] == fingerprint
              and isinstance(release['size'], int) and 0 < release['size'] <= MAX_IMAGE)
    except (KeyError, TypeError):
        ok = False
    if not ok:
        raise Refused('the beta descriptor does not match the updater contract')
    return {name: release[name] for name in RELEASE_FIELDS}


def fetch(settings, key, path, limit):
    """Download ORIGIN/key into path, at most `limit` bytes. Returns Last-Modified."""
    request = urllib.request.Request(settings.origin + key, headers={'User-Agent': 'okilum-beta-deliver'})
    with urllib.request.urlopen(request, timeout=60) as response, open(path, 'wb') as out:
        total = 0
        for block in iter(lambda: response.read(1 << 20), b''):
            total += len(block)
            if total > limit:
                raise Refused(f'{key} is larger than {limit} bytes')
            out.write(block)
        modified = response.headers.get('Last-Modified')
    if not modified:
        return None
    with contextlib.suppress(TypeError, ValueError):
        return email.utils.parsedate_to_datetime(modified).astimezone()
    return None


def descriptor(settings, scratch):
    data = Path(scratch) / 'release.json'
    sig = Path(scratch) / 'release.json.sig'
    key = f'okilum/linux/{CHANNEL}/latest.json'
    published = fetch(settings, key, data, 64 * 1024)
    fetch(settings, key + '.sig', sig, 16 * 1024)
    verify_signature(settings, data, sig)
    return validate_release(json.loads(data.read_text())), published


def download_payload(settings, release):
    """Verified payload in the private cache: (image, signature, provenance)."""
    folder = settings.cache / str(release['build'])
    image, sig, provenance = folder / 'new.AppImage', folder / 'payload.sig', folder / 'provenance.json'
    marker = folder / 'verified'
    if marker.is_file() and marker.read_text().strip() == release['sha256'] and image.is_file() \
            and image.stat().st_size == release['size'] and sha256(image) == release['sha256']:
        return image, sig, provenance
    if settings.cache.exists():
        for old in settings.cache.iterdir():
            if old.name != folder.name:
                shutil.rmtree(old, ignore_errors=True)
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True, mode=0o700)
    fetch(settings, release['key'], image, release['size'])
    fetch(settings, release['key'] + '.sig', sig, 16 * 1024)
    verify_signature(settings, image, sig)
    if image.stat().st_size != release['size'] or sha256(image) != release['sha256']:
        raise Refused('AppImage size/checksum mismatch')
    fetch(settings, release['key'].replace(IMAGE, 'provenance.json'), provenance, 64 * 1024)
    if sha256(provenance) != release['provenance_sha256']:
        raise Refused('provenance checksum mismatch')
    identity = json.loads(provenance.read_text())
    if not (identity.get('source_sha') == release['source']
            and identity.get('build_version') == str(release['build'])
            and identity.get('release_version') == release['version']
            and identity.get('sha256') == release['sha256']):
        raise Refused('payload provenance identity mismatch')
    marker.write_text(release['sha256'] + '\n')
    return image, sig, provenance


# --------------------------------------------------- installation records

def identity(stat_result):
    return f'{stat_result.st_size} {stat_result.st_mtime_ns // 10**9} {stat_result.st_mtime_ns % 10**9} {stat_result.st_ino}'


def owned(image):
    marker = image.with_name('.appimage-owner')
    try:
        st = os.lstat(marker)
        return stat_regular(st) and marker.read_bytes() == OWNER
    except OSError:
        return False


def stat_regular(st):
    return stat_module.S_ISREG(st.st_mode) and st.st_uid == os.geteuid() and st.st_nlink == 1


def recorded_build(image):
    """installation.rs recorded_build: the record must match the file's identity."""
    record = image.with_name('.appimage-build')
    try:
        if not stat_regular(os.lstat(record)):
            return None
        build, recorded = record.read_text().strip().split(' ', 1)
        if recorded != identity(os.lstat(image)):
            return None
        return int(build)
    except (OSError, ValueError):
        return None


def write_build(image, build, data):
    """installation.rs write_build: `data` is, or is about to be renamed to, image."""
    text = f'{build} {identity(os.lstat(data))}'
    fd, name = tempfile.mkstemp(dir=image.parent, prefix='.okilum-deliver-record.')
    with os.fdopen(fd, 'w') as out:
        out.write(text)
        out.flush()
        os.fsync(out.fileno())
    os.replace(name, image.with_name('.appimage-build'))


def installed_build(image, release):
    """Build at the managed path; unknown bytes equal to the release count as it."""
    build = recorded_build(image)
    if build is not None:
        return build
    try:
        if os.lstat(image).st_size == release['size'] and sha256(image) == release['sha256']:
            return release['build']
    except OSError:
        return None
    return None


# ------------------------------------------------------ the running window

def process_stat(proc, pid):
    fields = (Path(proc) / str(pid) / 'stat').read_text().rsplit(')', 1)[1].split()
    return fields[0], int(fields[19])


def environment(proc, pid):
    raw = (Path(proc) / str(pid) / 'environ').read_bytes()
    pairs = (item.split(b'=', 1) for item in raw.split(b'\0') if b'=' in item)
    return {k.decode(errors='surrogateescape'): v.decode(errors='surrogateescape') for k, v in pairs}


def gui_processes(proc, lock):
    """Okilum windows of this state directory: executable `okilum` holding exactly
    `lock` (okilum-dev-linux.sh). `okilum sync serve` and the Sprint build
    (its own state directory) never match."""
    found = []
    for name in sorted(os.listdir(proc), key=lambda n: (len(n), n)):
        if not name.isdigit():
            continue
        base = Path(proc) / name
        try:
            if base.stat().st_uid != os.getuid():
                continue
            exe = os.readlink(base / 'exe').split(' (deleted)')[0]
            if os.path.basename(exe) != 'okilum':
                continue
            holds = False
            for fd in os.listdir(base / 'fd'):
                with contextlib.suppress(OSError):
                    if os.readlink(base / 'fd' / fd).split(' (deleted)')[0] == lock:
                        holds = True
                        break
            if holds:
                found.append({'pid': int(name), 'start': process_stat(proc, name)[1], 'exe': exe,
                              'appimage': environment(proc, name).get('APPIMAGE')})
        except (OSError, ValueError, IndexError):
            continue
    return found


def same_process(proc, window):
    """/proc/<pid> still names the scanned window: same uid, executable and start
    time, not exited (a zombie has)."""
    base = Path(proc) / str(window['pid'])
    try:
        if base.stat().st_uid != os.getuid():
            return False
        exe = os.readlink(base / 'exe').split(' (deleted)')[0]
        state, start = process_stat(proc, window['pid'])
    except (OSError, ValueError, IndexError):
        return False
    return exe == window['exe'] and start == window['start'] and state not in ('Z', 'X')


# Thin wrappers so tests can stand in for the kernel.
def open_pidfd(pid):
    return os.pidfd_open(pid)


def close_pidfd(fd):
    os.close(fd)


def send_signal(fd, sig):
    signal.pidfd_send_signal(fd, sig)


def wait_exit(fd, seconds):
    """True once the pinned process has exited (a pidfd turns readable)."""
    poller = select.poll()
    poller.register(fd, select.POLLIN)
    return bool(poller.poll(max(0, int(seconds * 1000))))


def pin(proc, window):
    """A pidfd for the scanned window, or None if that pid now names another
    process. The identity is checked after opening: a match then proves the
    pidfd holds the scanned process, and a pidfd never follows a reused pid."""
    try:
        fd = open_pidfd(window['pid'])
    except ProcessLookupError:
        return None
    if same_process(proc, window):
        return fd
    close_pidfd(fd)
    return None


def quit_app(proc, lock, image, watch, term_wait, monotonic=time.monotonic):
    """Quit the Okilum windows of `image` only while the user is idle.

    Windows are resolved again now (the idle wait can last 25 min) and pinned by
    pidfd to uid + executable + start time. SIGTERM is the app's Quit
    (reader_recovery.rs: editors saved, then the launch marker is cleared), and it
    is sent only if no input arrived up to this moment. There is no SIGKILL: the
    app exposes no "everything is saved" state while it runs (its launch marker
    is cleared only by a safe shutdown), so a window that does not quit within
    the grace keeps running and the next timer run tries again.

    Returns 'TERM' (every window exited), 'closed' (none left to quit),
    'user active' (input before the signal, nothing sent) or 'still running'."""
    fresh = [w for w in gui_processes(proc, lock) if w['appimage'] == str(image)]
    pinned = []
    try:
        for window in fresh:
            fd = pin(proc, window)
            if fd is not None:
                pinned.append(fd)
        if not pinned:
            return 'closed'
        try:
            idle = watch.still_idle()
        except (OSError, ConnectionError):
            idle = False                                 # cannot tell: never signal
        if not idle:
            return 'user active'
        for fd in pinned:
            with contextlib.suppress(ProcessLookupError):
                send_signal(fd, signal.SIGTERM)
        deadline = monotonic() + term_wait
        for fd in pinned:
            if not wait_exit(fd, deadline - monotonic()):
                return 'still running'
        return 'TERM'
    finally:
        for fd in pinned:
            close_pidfd(fd)


# ------------------------------------------------------- Wayland idleness

def wl_string(text):
    data = text.encode() + b'\0'
    return struct.pack('<I', len(data)) + data + b'\0' * (-len(data) % 4)


def wl_message(obj, opcode, payload=b''):
    return struct.pack('<II', obj, ((len(payload) + 8) << 16) | opcode) + payload


class Wayland:
    """The few wire-protocol requests needed for ext-idle-notify-v1, no libraries."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10)
        self.sock.connect(path)
        self.buffer = b''
        self.next_id = 2

    def close(self):
        self.sock.close()

    def new_id(self):
        value = self.next_id
        self.next_id += 1
        return value

    def send(self, obj, opcode, payload=b''):
        self.sock.sendall(wl_message(obj, opcode, payload))

    def _take(self):
        """One complete message from the buffer, or None."""
        if len(self.buffer) < 8 or len(self.buffer) < (struct.unpack_from('<I', self.buffer, 4)[0] >> 16):
            return None
        obj, header = struct.unpack_from('<II', self.buffer)
        size, opcode = header >> 16, header & 0xFFFF
        payload, self.buffer = self.buffer[8:size], self.buffer[size:]
        if obj == 1 and opcode == 0:
            message = payload[12:12 + struct.unpack_from('<I', payload, 8)[0] - 1].decode(errors='replace')
            raise ConnectionError(f'Wayland protocol error: {message}')
        return obj, opcode, payload

    def _read(self, timeout):
        ready, _, _ = select.select([self.sock], [], [], max(0, timeout))
        if not ready:
            return False
        chunk = self.sock.recv(65536)
        if not chunk:
            raise ConnectionError('Wayland compositor closed the connection')
        self.buffer += chunk
        return True

    def event(self, deadline):
        """(object, opcode, payload), or None at the deadline."""
        while True:
            message = self._take()
            if message is not None:
                return message
            left = deadline - time.monotonic()
            if left <= 0 or not self._read(left):
                return None

    def pending(self):
        """Every message the compositor has sent so far, in order, without waiting."""
        while self._read(0):
            pass
        messages = []
        while (message := self._take()) is not None:
            messages.append(message)
        return messages


def parse_global(payload):
    name, length = struct.unpack_from('<II', payload)
    interface = payload[8:8 + length - 1].decode()
    offset = 8 + length + (-length % 4)
    return name, interface, struct.unpack_from('<I', payload, offset)[0]


class IdleWatch:
    """An ext-idle-notify-v1 notification kept open for the whole quit, so input
    at any moment counts. `idled`/`resumed` are applied strictly in order; the
    user is idle only if the last event received so far is `idled`."""

    def __init__(self, path, idle_ms, connect=Wayland):
        self.wl = connect(path)
        self.idle = False
        try:
            self.notification = self._subscribe(idle_ms)
        except BaseException:
            self.wl.close()
            raise

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.wl.close()

    def _subscribe(self, idle_ms):
        wl = self.wl
        registry, callback = wl.new_id(), wl.new_id()
        wl.send(1, 1, struct.pack('<I', registry))      # wl_display.get_registry
        wl.send(1, 0, struct.pack('<I', callback))      # wl_display.sync
        globals_ = {}
        deadline = time.monotonic() + 10
        while True:
            event = wl.event(deadline)
            if event is None:
                raise ConnectionError('Wayland registry did not answer')
            obj, opcode, payload = event
            if obj == registry and opcode == 0:
                name, interface, version = parse_global(payload)
                globals_.setdefault(interface, (name, version))
            elif obj == callback and opcode == 0:
                break
        if 'ext_idle_notifier_v1' not in globals_ or 'wl_seat' not in globals_:
            raise ConnectionError('the compositor offers no ext_idle_notifier_v1')
        bound = {}
        for interface, wanted in (('ext_idle_notifier_v1', 2), ('wl_seat', 1)):
            name, version = globals_[interface]
            version = min(version, wanted)
            bound[interface] = (wl.new_id(), version)
            wl.send(registry, 0, struct.pack('<I', name) + wl_string(interface)
                    + struct.pack('<II', version, bound[interface][0]))
        notifier, version = bound['ext_idle_notifier_v1']
        notification = wl.new_id()
        # get_input_idle_notification (v2) ignores idle inhibitors such as video.
        wl.send(notifier, 2 if version >= 2 else 1,
                struct.pack('<III', notification, idle_ms, bound['wl_seat'][0]))
        return notification

    def _apply(self, event):
        obj, opcode, _ = event
        if obj == self.notification and opcode in (0, 1):
            self.idle = opcode == 0                      # 0 idled, 1 resumed

    def still_idle(self):
        """Apply everything received up to now; True only if still idle."""
        for event in self.wl.pending():
            self._apply(event)
        return self.idle

    def wait(self, seconds):
        """True once idle with no later `resumed` already received; False at the deadline."""
        deadline = time.monotonic() + seconds
        while True:
            if self.still_idle():
                return True
            event = self.wl.event(deadline)
            if event is None:
                return False
            self._apply(event)


def wait_idle(path, idle_ms, wait_seconds, connect=Wayland):
    """One-shot form of IdleWatch.wait."""
    with IdleWatch(path, idle_ms, connect) as watch:
        return watch.wait(wait_seconds)


# ------------------------------------------------------------- the install

def take_lock(path, mode, deadline, private=False):
    """flock `path` exclusively, waiting until `deadline`; returns the fd."""
    flags = os.O_RDWR | os.O_CREAT | (os.O_NOFOLLOW if private else 0)
    try:
        fd = os.open(path, flags, mode)
    except OSError as error:
        raise Refused(f'cannot open {Path(path).name}: {error.strerror}') from None
    try:
        st = os.fstat(fd)
        if private and (st.st_nlink != 1 or st.st_uid != os.geteuid() or st.st_mode & 0o077):
            raise Refused('unsafe AppImage update lock')
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return fd
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise Refused('an Okilum install or update holds its lock') from None
                time.sleep(0.5)
    except BaseException:
        os.close(fd)
        raise


@contextlib.contextmanager
def install_locks(image, seconds):
    """Okilum's installer and updater locks (installation.rs, lib.rs update_lock),
    in their order; a staged or applying in-app update is never overwritten.
    Yields a function that releases the install lock early: like the APPLY
    helper, only the update lock is held while the new build starts, because
    an ordinary launch takes the install lock and blocks on it (#2455 path)."""
    deadline = time.monotonic() + seconds
    install = take_lock(image.parent / '.install-lock', 0o644, deadline)
    held = [install]

    def release_install():
        if install in held:
            held.remove(install)
            os.close(install)
    try:
        held.append(take_lock(image.parent / f'.{image.name}.update-lock', 0o600, deadline, private=True))
        yield release_install
    finally:
        for fd in held:
            os.close(fd)


def private_stages(parent):
    stages = []
    for entry in os.scandir(parent):
        if not entry.name.startswith('.okilum-update-'):
            continue
        st = os.lstat(entry.path)
        if stat_module.S_ISDIR(st.st_mode) and st.st_uid == os.geteuid() and st.st_mode & 0o777 == 0o700:
            stages.append(Path(entry.path))
    return stages


def recover_interrupted(image):
    """lib.rs recover_interrupted_at: drop an abandoned previous.AppImage link."""
    current = os.lstat(image)
    if current.st_nlink == 1:
        return
    for stage in private_stages(image.parent):
        previous = stage / 'previous.AppImage'
        with contextlib.suppress(FileNotFoundError):
            old = os.lstat(previous)
            if old.st_dev == current.st_dev and old.st_ino == current.st_ino:
                previous.unlink()


def prune_stages(parent):
    """lib.rs prune_stages: keep the newest completed and newest interrupted stage."""
    completed, interrupted = [], []
    stages = private_stages(parent)
    for stage in stages:
        if (stage / 'completed').is_file() and not (stage / 'completed').is_symlink():
            completed.append((os.lstat(stage / 'completed').st_mtime_ns, str(stage)))
        elif (stage / 'previous.AppImage').is_file() and not (stage / 'previous.AppImage').is_symlink():
            interrupted.append((os.lstat(stage).st_mtime_ns, str(stage)))
    keep = {max(completed)[1] if completed else None, max(interrupted)[1] if interrupted else None}
    for stage in stages:
        if str(stage) not in keep:
            shutil.rmtree(stage)


def atomic_write(path, text):
    temporary = path.with_name(path.name + '.tmp')
    with open(temporary, 'w') as out:
        out.write(text)
        out.flush()
        os.fsync(out.fileno())
    os.replace(temporary, path)
    fsync_dir(path.parent)


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def apply(settings, release, payload):
    """Replace the managed AppImage with the APPLY transaction of lib.rs.
    Returns (stage, previous build or None)."""
    image = settings.image
    st = os.lstat(image)
    if not stat_module.S_ISREG(st.st_mode) or st.st_uid != os.geteuid():
        raise Refused(f'{image} is not a regular file owned by you')
    recover_interrupted(image)
    if os.lstat(image).st_nlink != 1:
        raise Refused(f'{image} has other hard links')
    previous_build = recorded_build(image)
    original = sha256(image)
    prune_stages(image.parent)
    stage = Path(tempfile.mkdtemp(prefix='.okilum-update-', dir=image.parent))
    os.chmod(stage, 0o700)
    new = stage / 'new.AppImage'
    image_file, signature, provenance = payload
    shutil.copyfile(image_file, new)
    shutil.copyfile(signature, stage / 'payload.sig')
    shutil.copyfile(provenance, stage / 'provenance.json')
    os.chmod(new, st.st_mode & 0o777)
    with open(new, 'rb') as handle:
        os.fsync(handle.fileno())
    if sha256(new) != release['sha256']:
        raise Refused('staged payload changed')
    if owned(image):
        write_build(image, release['build'], new)   # installation.rs note_incoming
    os.link(image, stage / 'previous.AppImage')
    atomic_write(stage / 'expected.json', json.dumps(release, separators=(',', ':')))
    with open(stage / 'apply.log', 'a') as log:
        if not os.path.samefile(image, stage / 'previous.AppImage') or sha256(stage / 'previous.AppImage') != original:
            os.unlink(stage / 'previous.AppImage')
            raise Refused('original AppImage changed; refusing apply')
        if sha256(new) != release['sha256']:
            os.unlink(stage / 'previous.AppImage')
            raise Refused('staged payload changed')
        os.replace(new, image)
        fsync_dir(image.parent)
        log.write(f'Applied build {release["build"]} by okilum-beta-deliver; previous.AppImage is the manual rollback copy\n')
    return stage, previous_build


def roll_back(settings, release, stage, previous_build):
    """APPLY's rollback: only our bytes are replaced, never another install."""
    image = settings.image
    if sha256(image) != release['sha256']:
        return False
    os.replace(stage / 'previous.AppImage', image)
    fsync_dir(image.parent)
    if previous_build is not None and owned(image):
        write_build(image, previous_build, image)
    with open(stage / 'apply.log', 'a') as log:
        log.write('Updated application exited before startup confirmation; previous AppImage restored\n')
    return True


# ---------------------------------------------------------------- launch

def launch(image, receipt=None, run=subprocess.run, suffix=None):
    """Open Okilum in its own transient unit in the app slice, as a launcher
    (uwsm) does: a child of this service would die with the service. The unit
    gets the session's environment from the user manager."""
    unit = f'app-okilum-beta-deliver-{suffix or time.time_ns()}.service'
    command = ['systemd-run', '--user', '--quiet', '--collect', '--unit', unit,
               '--slice', 'app-graphical.slice', '--property', 'Type=exec',
               # UnsetEnvironment= applies after --setenv: never list the receipt there.
               '--property', 'UnsetEnvironment=' + ' '.join(
                   name for name in RUNTIME_ENV if not (receipt and name == 'OKILUM_APPIMAGE_UPDATE_RECEIPT')),
               '--description', 'Okilum (reopened by okilum-beta-deliver)']
    if receipt:
        command += ['--setenv', f'OKILUM_APPIMAGE_UPDATE_RECEIPT={receipt}']
    command += ['--', str(image)]
    result = run(command, capture_output=True, text=True)
    if result.returncode != 0:
        raise Refused(f'systemd-run failed: {result.stderr.strip()}')
    return unit


def unit_active(unit, run=subprocess.run):
    result = run(['systemctl', '--user', 'is-active', '--quiet', unit])
    return result.returncode == 0


def wait_started(stage, unit, seconds, active=None, sleep=time.sleep, monotonic=time.monotonic):
    """'started' (the app wrote its receipt), 'exited' or 'timeout'."""
    active = active or unit_active
    deadline = monotonic() + seconds
    while True:
        if (stage / 'started').is_file():
            return 'started'
        if not active(unit):
            return 'started' if (stage / 'started').is_file() else 'exited'
        if monotonic() >= deadline:
            return 'timeout'
        sleep(0.5)


# ----------------------------------------------------------------- ledger

def failures(settings, build):
    try:
        return int((settings.own_state / f'failed-{build}').read_text())
    except (OSError, ValueError):
        return 0


def note_failure(settings, build):
    settings.own_state.mkdir(parents=True, exist_ok=True)
    (settings.own_state / f'failed-{build}').write_text(str(failures(settings, build) + 1))


def note_check(settings, **fields):
    """Heartbeat for the manager without a log line every few minutes."""
    settings.own_state.mkdir(parents=True, exist_ok=True)
    fields['time'] = now().isoformat(timespec='seconds')
    atomic_write(settings.own_state / 'last-check.json', json.dumps(fields, sort_keys=True) + '\n')


def note_once(settings, key, text):
    """Log a waiting state once per build, not on every timer tick."""
    settings.own_state.mkdir(parents=True, exist_ok=True)
    flag = settings.own_state / f'noted-{key}'
    if not flag.exists():
        flag.write_text(text)
        log_line(settings, text)


# ------------------------------------------------------------------- main

def deliver(settings, proc='/proc'):
    if not settings.image.is_file():
        return 'no managed Okilum AppImage'
    with tempfile.TemporaryDirectory(prefix='okilum-deliver.') as scratch:
        release, published = descriptor(settings, scratch)
    build = release['build']
    current = installed_build(settings.image, release)
    note_check(settings, feed=build, installed=current)
    if current is not None and current >= build:
        return f'up to date ({current})'
    if failures(settings, build) >= settings.max_failures:
        note_once(settings, f'gave-up-{build}', f'skipped build {build}: {settings.max_failures} failed attempts')
        return 'gave up on this build'
    seen = now()
    payload = download_payload(settings, release)
    lock = str(settings.app_state / INSTANCE_LOCK)
    windows = gui_processes(proc, lock)
    foreign = [w for w in windows if w['appimage'] != str(settings.image)]
    windows = [w for w in windows if w['appimage'] == str(settings.image)]
    for window in foreign:
        log_line(settings, f'Okilum pid {window["pid"]} runs {window["appimage"] or window["exe"]}, not the managed AppImage: left running')
    quit_how, idle_wait = 'not running', 0
    if windows:
        if not settings.wayland:
            return 'no Wayland session to measure idleness'
        idle_started = time.monotonic()
        try:
            watch = IdleWatch(settings.wayland, settings.idle_ms)
        except (OSError, ConnectionError) as error:
            note_once(settings, f'idle-error-{build}', f'build {build} waits: cannot measure idleness ({error})')
            return 'idleness unknown'
        with watch:
            try:
                idle = watch.wait(settings.idle_wait)
            except (OSError, ConnectionError) as error:
                note_once(settings, f'idle-error-{build}', f'build {build} waits: cannot measure idleness ({error})')
                return 'idleness unknown'
            if idle:
                # An in-app update may have landed during the wait.
                current = installed_build(settings.image, release)
                if current is not None and current >= build:
                    return f'up to date ({current})'
                quit_how = quit_app(proc, lock, settings.image, watch, settings.term_wait)
        if not idle:
            note_once(settings, f'busy-{build}', f'build {build} waits: user active for {settings.idle_wait:.0f} s')
            return 'user active'
        if quit_how == 'user active':
            note_once(settings, f'resumed-{build}', f'build {build} waits: input arrived just before quitting; nothing sent')
            return 'user active'
        if quit_how == 'still running':
            note_once(settings, f'noquit-{build}', f'build {build} waits: Okilum did not quit {settings.term_wait:.0f} s'
                      ' after SIGTERM (a dialog?); left running, never killed')
            return 'Okilum did not quit'
        if quit_how == 'closed':
            windows = []
        idle_wait = int(time.monotonic() - idle_started)
    try:
        return install(settings, release, payload, windows, published, seen, idle_wait, quit_how)
    except BaseException:
        # Okilum was quit for this install: never leave the user without it.
        if windows and not gui_processes(proc, lock):
            with contextlib.suppress(Exception):
                log_line(settings, f'install of build {build} failed after quitting Okilum; reopened (unit {launch(settings.image)})')
        raise


def install(settings, release, payload, windows, published, seen, idle_wait, quit_how):
    build = release['build']
    summary = (f'build {build} source {release["source"][:9]} · beta published {clock(published)}'
               f' · seen {clock(seen)} · idle wait {idle_wait}s · quit {quit_how}')
    restored, newer = False, None
    with install_locks(settings.image, settings.lock_wait) as release_install:
        # Under both locks: an in-app update made while this run waited is never
        # replaced by an older or equal build.
        current = installed_build(settings.image, release)
        if current is not None and current >= build:
            newer = current
        else:
            stage, previous_build = apply(settings, release, payload)
            release_install()
            installed = now()
            if not windows:
                atomic_write(stage / 'completed', 'committed\n')
                log_line(settings, f'installed {summary} · installed {clock(installed)} · Okilum was not open, not started')
                forget_older(settings, build)
                return f'installed {build}'
            try:
                unit = launch(settings.image, receipt=stage)
                outcome = wait_started(stage, unit, settings.start_wait)
            except Refused as error:
                unit, outcome = str(error), 'exited'
            if outcome == 'started':
                atomic_write(stage / 'completed', 'committed\n')
                log_line(settings, f'installed {summary} · installed {clock(installed)} · running {clock(now())}'
                         f' (start confirmed by the app, unit {unit})')
                forget_older(settings, build)
                return f'installed {build}'
            note_failure(settings, build)
            if outcome == 'timeout':
                log_line(settings, f'FAILED {summary} · installed {clock(installed)} · no start confirmation in'
                         f' {settings.start_wait:.0f} s; it keeps running, previous copy in {stage}')
                return 'unconfirmed'
            restored = roll_back(settings, release, stage, previous_build)
    if newer is not None:
        reopened = f', reopened (unit {launch(settings.image)})' if windows else ''
        log_line(settings, f'build {build} not installed: build {newer} is already installed{reopened}')
        return f'up to date ({newer})'
    reopened = launch(settings.image)
    log_line(settings, f'FAILED {summary} · exited before confirming its start ({unit}); '
             + ('previous AppImage restored' if restored else 'AppImage changed meanwhile, left as is')
             + f', reopened (unit {reopened})')
    return 'rolled back'


def forget_older(settings, build):
    """Drop waiting/failure notes of builds up to the installed one."""
    for note in settings.own_state.glob('*-*'):
        number = note.name.rsplit('-', 1)[-1]
        if number.isdigit() and int(number) <= build:
            note.unlink(missing_ok=True)


@contextlib.contextmanager
def single_run(settings):
    settings.own_state.mkdir(parents=True, exist_ok=True)
    fd = os.open(settings.own_state / 'run.lock', os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        os.close(fd)
        yield False
        return
    try:
        yield True
    finally:
        os.close(fd)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n', 1)[0])
    parser.add_argument('--check', action='store_true', help='print feed and installed build, change nothing')
    args = parser.parse_args(argv)
    settings = Settings()
    if args.check:
        with tempfile.TemporaryDirectory(prefix='okilum-deliver.') as scratch:
            release, published = descriptor(settings, scratch)
        print(json.dumps({'feed': release['build'], 'published': published.isoformat() if published else None,
                          'installed': installed_build(settings.image, release) if settings.image.is_file() else None}))
        return 0
    with single_run(settings) as mine:
        if not mine:
            print('another okilum-beta-deliver run is active')
            return 0
        try:
            print(deliver(settings))
        except Refused as error:
            log_line(settings, f'refused: {error}')
            return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
