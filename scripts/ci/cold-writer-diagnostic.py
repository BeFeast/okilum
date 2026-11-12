"""Observe the unchanged canonical prelude and its first compile-inclusive group."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time

LIMIT = 900
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('test_group', HERE / 'test-group.py')
group = importlib.util.module_from_spec(spec)
spec.loader.exec_module(group)
CGROUP_FILES = ('cpu.max', 'cpu.stat', 'cpu.pressure', 'cpuset.cpus.effective',
                'memory.max', 'memory.current', 'memory.peak', 'memory.events',
                'memory.pressure', 'io.stat', 'io.pressure', 'cpu.cfs_quota_us',
                'cpu.cfs_period_us', 'cpuacct.usage', 'cpuset.cpus',
                'memory.limit_in_bytes', 'memory.usage_in_bytes', 'memory.failcnt')


def read(path):
    try:
        return Path(path).read_text()
    except (OSError, UnicodeError):
        return None


def cgroups():
    root = Path('/sys/fs/cgroup')
    paths = {root}
    for line in (read('/proc/self/cgroup') or '').splitlines():
        _, controllers, relative = line.split(':', 2)
        bases = [root] + [root / name for name in controllers.split(',') if name]
        if controllers:
            bases.append(root / controllers)
        for base in bases:
            candidate = base / relative.lstrip('/')
            if candidate.is_dir():
                paths.add(candidate)
    return [dict(directory=str(path), **{name: value for name in CGROUP_FILES
             if (value := read(path / name)) is not None}) for path in sorted(paths)]


def snapshot():
    processes = []
    for pid in group.descendants():
        raw = read(f'/proc/{pid}/stat')
        if raw is None:
            continue
        fields = raw.rsplit(')', 1)[1].split()
        processes.append(dict(pid=pid, name=raw.split('(', 1)[1].rsplit(')', 1)[0],
                              command=(read(f'/proc/{pid}/cmdline') or '').rstrip('\0').split('\0'),
                              start_ticks=fields[19], user_ticks=int(fields[11]),
                              system_ticks=int(fields[12]), rss_pages=int(fields[21]),
                              major_faults=int(fields[9]), io=read(f'/proc/{pid}/io')))
    return dict(monotonic=time.monotonic(), cgroups=cgroups(), processes=processes)


def emit(kind, value):
    payload = json.dumps(value, sort_keys=True)
    # Concurrent rustc argv can exceed the runner's 64 KiB log-line limit.
    # Preserve the full sample in ordered, independently valid JSON chunks.
    if len(payload) > 12000:
        chunks = [payload[start:start + 4096] for start in range(0, len(payload), 4096)]
        for index, data in enumerate(chunks):
            print(kind + '_CHUNK ' + json.dumps(dict(index=index, count=len(chunks), data=data)),
                  flush=True)
    else:
        print(kind + ' ' + payload, flush=True)


def observe(command, env, interval=5):
    process = subprocess.Popen(command, env=env)
    previous = {}
    def cancel(number, frame):
        if process.poll() is None:
            process.send_signal(number)  # Existing supervisor owns descendant cleanup.
    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        previous[number] = signal.signal(number, cancel)
    try:
        while True:
            emit('COLD_WRITER_SAMPLE', snapshot())
            try:
                status = process.wait(timeout=interval)
                emit('COLD_WRITER_FINAL', dict(status=status, **snapshot()))
                return status
            except subprocess.TimeoutExpired:
                pass
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=32)


def identity(source):
    versions = {}
    for name, command in [('rustc', ['rustc', '-vV']), ('cargo', ['cargo', '-V']),
                          ('linker', ['mold', '--version'])]:
        result = subprocess.run(command, capture_output=True, text=True, timeout=15)
        versions[name] = dict(status=result.returncode, text=result.stdout[:4096])
    return dict(source=source, diagnostic_source=os.environ.get('COLD_WRITER_TOOL_SOURCE'),
                diagnostic_ref=os.environ.get('COLD_WRITER_TOOL_REF'),
                versions=versions, cpu_count=os.cpu_count(),
                affinity=sorted(os.sched_getaffinity(0)), cgroups=cgroups(),
                page_size=os.sysconf('SC_PAGE_SIZE'), clock_ticks=os.sysconf('SC_CLK_TCK'),
                settings={name: os.environ.get(name) for name in (
                    'CARGO_BUILD_JOBS', 'CARGO_INCREMENTAL', 'CARGO_PROFILE_DEV_DEBUG',
                    'CARGO_PROFILE_TEST_DEBUG', 'CARGO_PROFILE_TEST_CODEGEN_UNITS',
                    'RUSTFLAGS', 'RUSTC_WRAPPER')})


def admission(source, marker):
    marker.write_text(json.dumps(dict(source=source, limit=LIMIT)))
    emit('COLD_WRITER_IDENTITY', identity(source))
    # Keep Cargo's ordinary target headers: -vv changes them into shell commands
    # that the original positive target guards intentionally do not accept.
    # Compiler/linker argv are sampled from /proc without altering dispatch.
    return observe(['bash', 'scripts/ci/test-group.sh', 'cold-writer', str(LIMIT),
                    'bash', 'scripts/ci/local-writer-admission.sh'], dict(os.environ))


def prelude(source):
    root = Path.cwd().resolve()
    with tempfile.TemporaryDirectory(prefix='writer-boundary-') as directory:
        marker = Path(directory) / 'invoked.json'
        startup = Path(directory) / 'boundary.bash'
        command = shlex.join([sys.executable, str(Path(__file__).resolve()),
                              '--source', source, '--admission', '--marker', str(marker)])
        # Keep PATH's bash a real executable: the original prelude copies it
        # as a window stand-in and checks its executable identity in /proc.
        # Install this function only in the top-level measured script. Child
        # shells inherit BASH_ENV, but neither this function nor its dispatch.
        startup.write_text(
            f'if [[ $0 == scripts/ci/check-linux.sh && $PWD == {shlex.quote(str(root))} ]]; then\n'
            '  bash() {\n'
            '    if [[ $# == 1 && $1 == scripts/ci/linux-rust-tests.sh ]]; then\n'
            f'      exec {command}\n'
            '    fi\n'
            '    command bash "$@"\n'
            '  }\n'
            'fi\n')
        # Reproduce the original check-linux prelude, rather than inserting a
        # new build outside the 900s budget. Stop after its original first group
        # so a timeout cannot cascade into repeatedly restarted compilation.
        result = subprocess.run(['/bin/bash', 'scripts/ci/check-linux.sh'],
            env=dict(os.environ, BASH_ENV=str(startup),
                     OKILUM_SOURCE_COMMIT=source, GITHUB_SHA=source,
                     GITHUB_REF='refs/heads/main'))
        invoked = marker.exists()
        emit('COLD_WRITER_BOUNDARY', dict(source=source, invoked=invoked,
                                        status=result.returncode))
        return result.returncode if invoked else result.returncode or 125


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--admission', action='store_true')
    parser.add_argument('--marker', type=Path)
    args = parser.parse_args()
    actual = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if len(args.source) != 40 or actual != args.source:
        parser.error('source must be the exact checked-out commit')
    if args.admission:
        if args.marker is None:
            parser.error('admission requires its boundary receipt')
        return admission(args.source, args.marker)
    return prelude(args.source)


if __name__ == '__main__':
    raise SystemExit(main())
