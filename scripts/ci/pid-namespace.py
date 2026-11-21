#!/usr/bin/env python3
"""Run one command alone in a private PID namespace, as the calling user.

The managed-admission HOST_DISCOVERY test scans /proc for every same-uid
Syncthing and refuses any it does not own (#3069). On a build-slot host the
other slots run as the same user without their own PID namespace, so a sibling
shard's fixture daemon would fail that test on healthy code (#3186 review).
Inside `unshare --pid --mount-proc` the test sees only its own processes; the
product assertion itself is unchanged.

Unprivileged user namespaces are blocked on some pool hosts (baldr), and sudo
there resets the environment, so the outer side writes the environment to a
private file and runs

    sudo -n unshare --pid --fork --kill-child --mount-proc --
        setpriv --reuid UID --regid GID --init-groups --
        python3 -I pid-namespace.py --inner ENVFILE -- CMD...

The inner side restores the environment, deletes the file, checks that no
same-uid Syncthing is visible, then stays PID 1: it forwards termination
signals to CMD, reaps every orphan and exits with CMD's status.
`--probe` is the positive control used before every isolated run: a decoy
process named syncthing outside must be visible here and invisible inside.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid

HERE = Path(__file__).resolve()
MARKER = 'OKILUM_PIDNS_TOKEN'


def same_uid_syncthing(proc='/proc'):
    """The scope of direct::configuration_paths: this uid, an exe named syncthing."""
    found = []
    for entry in Path(proc).iterdir():
        try:
            if not entry.name.isdecimal() or entry.stat().st_uid != os.geteuid():
                continue
            if os.readlink(entry / 'exe').removesuffix(' (deleted)').rsplit('/', 1)[-1] == 'syncthing':
                found.append(int(entry.name))
        except (FileNotFoundError, ProcessLookupError, PermissionError, NotADirectoryError):
            continue
    return found


def command(argv, env, workdir, inner_args=()):
    """(argv, env file, token) for one isolated run; the env file is private to the caller."""
    token = uuid.uuid4().hex
    env = dict(env, **{MARKER: token})
    fd, path = tempfile.mkstemp(prefix='pidns-env-', suffix='.json', dir=workdir)
    with os.fdopen(fd, 'w') as handle:
        json.dump(env, handle)
    wrapped = ['sudo', '-n', 'unshare', '--pid', '--fork', '--kill-child', '--mount-proc', '--',
               'setpriv', '--reuid', str(os.getuid()), '--regid', str(os.getgid()), '--init-groups', '--',
               sys.executable, '-I', str(HERE), '--inner', path, *inner_args, '--']
    return wrapped + list(argv), path, token


def kill_marked(token):
    """SIGKILL every same-uid process of one isolated run (the namespace init ends the rest)."""
    needle = f'{MARKER}={token}'.encode()
    for entry in Path('/proc').iterdir():
        try:
            if not entry.name.isdecimal() or entry.stat().st_uid != os.geteuid():
                continue
            if needle in (entry / 'environ').read_bytes().split(b'\0'):
                os.kill(int(entry.name), signal.SIGKILL)
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue


def probe(workdir):
    """Positive control: a same-uid decoy syncthing outside is invisible inside."""
    decoy_dir = Path(tempfile.mkdtemp(prefix='pidns-decoy-', dir=workdir))
    decoy_exe = decoy_dir / 'syncthing'
    # A copy of bash named syncthing waits on a pipe with a builtin, so the
    # decoy is one process whose /proc/PID/exe names syncthing (a multicall
    # coreutils copy would refuse to run under that name).
    shutil.copy2(shutil.which('bash'), decoy_exe)
    decoy = subprocess.Popen(['bash', '-c', 'read -r -t 120 _ || true'], executable=str(decoy_exe),
                             stdin=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while decoy.pid not in same_uid_syncthing() and time.monotonic() < deadline:
            time.sleep(.02)
        outside = same_uid_syncthing()
        assert decoy.poll() is None and decoy.pid in outside, (
            'decoy syncthing not visible outside the namespace', decoy.poll(), outside)
        argv, env_file, _ = command([], os.environ, workdir, ('--probe',))
        result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        Path(env_file).unlink(missing_ok=True)
        assert result.returncode == 0, ('isolated probe failed', result.returncode, result.stderr)
        inside = json.loads(result.stdout.strip().splitlines()[-1])
        assert inside == [], ('same-uid Syncthing visible inside the namespace', inside)
        assert decoy.poll() is None and decoy.pid in same_uid_syncthing(), 'decoy ended during the probe'
        print(f'PID_NAMESPACE_CONTROL: decoy syncthing {decoy.pid} visible outside, '
              f'invisible inside the isolated namespace', flush=True)
    finally:
        decoy.kill()
        decoy.wait()
        decoy.stdin.close()
        shutil.rmtree(decoy_dir, ignore_errors=True)


def inner(args):
    env_file, rest = args[0], args[1:]
    probe_only = bool(rest) and rest[0] == '--probe'
    if probe_only:
        rest = rest[1:]
    assert rest and rest[0] == '--', rest
    rest = rest[1:]
    env = json.loads(Path(env_file).read_text())
    Path(env_file).unlink()
    visible = same_uid_syncthing()
    if probe_only:
        print(json.dumps(visible), flush=True)
        return 0
    if os.getpid() != 1:
        print(f'pid-namespace: not the namespace init (pid {os.getpid()})', file=sys.stderr)
        return 70
    if visible:
        print(f'pid-namespace: same-uid Syncthing visible inside the namespace: {visible}', file=sys.stderr)
        return 70
    return init(rest, env)


def init(argv, env):
    """PID 1 of the namespace: run argv, forward signals, reap orphans."""
    child = os.fork()
    if child == 0:
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, signal.SIG_DFL)
        try:
            os.execvpe(argv[0], argv, env)
        except OSError as error:
            print(f'pid-namespace: cannot run {argv[0]}: {error}', file=sys.stderr)
            os._exit(127)
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, lambda signum, _frame: os.kill(child, signum))
    while True:
        try:
            pid, status = os.wait()
        except InterruptedError:
            continue
        if pid == child:
            # Leaving PID 1 ends every remaining process of the namespace.
            return os.waitstatus_to_exitcode(status) if os.WIFEXITED(status) else 128 + os.WTERMSIG(status)


if __name__ == '__main__':
    if len(sys.argv) > 2 and sys.argv[1] == '--inner':
        sys.exit(inner(sys.argv[2:]))
    print('usage: imported by sync-managed-admission-runs.py; --inner is internal', file=sys.stderr)
    sys.exit(64)
