#!/usr/bin/env python3
"""Host-load guard for the daily full set (Oleg 13.11 evening).

The full set runs at a fixed hour while baldr is idle, with budgets
unchanged. A run whose host load exceeded twice the core count is no
verdict, not a code verdict: its failures say nothing about the code.

    host-load.py start <samples.tsv> [--interval S]   # background sampler
    host-load.py verdict <samples.tsv> --job-status <success|failure|...>

/proc/loadavg is host-wide, also inside a job container, which is what the
rule is about. `verdict` never changes the job result; it reports.
"""
import argparse
import os
import signal
import sys
import time

FACTOR = 2.0


def read_load():
    one, five = open('/proc/loadavg').read().split()[:2]
    return float(one), float(five)


def host_cores():
    return os.cpu_count() or 1


def assess(rows, cores, factor=FACTOR):
    """rows: (unix_time, load1, load5). The 5-minute average is the measure."""
    peak = max((row[2] for row in rows), default=None)
    limit = factor * cores
    return {'samples': len(rows), 'peak_load5': peak, 'cores': cores,
            'limit': limit, 'overloaded': peak is not None and peak > limit}


def parse(path):
    rows = []
    try:
        with open(path) as handle:
            for line in handle:
                parts = line.split('\t')
                if len(parts) == 3:
                    rows.append((int(parts[0]), float(parts[1]), float(parts[2])))
    except FileNotFoundError:
        pass
    return rows


def record(path):
    one, five = read_load()
    with open(path, 'a') as handle:
        handle.write(f'{int(time.time())}\t{one}\t{five}\n')


def start(path, interval):
    record(path)
    pid = os.fork()
    if pid:
        with open(path + '.pid', 'w') as handle:
            handle.write(str(pid))
        return 0
    os.setsid()
    null = os.open(os.devnull, os.O_RDWR)
    for fd in (0, 1, 2):
        os.dup2(null, fd)
    while True:
        time.sleep(interval)
        try:
            record(path)
        except OSError:
            pass


def verdict(path, status, out=sys.stdout):
    try:
        with open(path + '.pid') as handle:
            os.kill(int(handle.read().strip()), signal.SIGTERM)
    except (OSError, ValueError):
        pass
    try:
        record(path)
    except OSError:
        pass
    result = assess(parse(path), host_cores())
    no_verdict = result['overloaded'] and status != 'success'
    peak = 'unknown' if result['peak_load5'] is None else f"{result['peak_load5']:.1f}"
    line = (f"Host load: peak 5-min average {peak} over {result['samples']} samples; "
            f"limit {result['limit']:.0f} (2 x {result['cores']} cores).")
    if no_verdict:
        message = ('NO VERDICT: the host was overloaded during this run; its failures '
                   'are not a code verdict (Oleg 13.11). ' + line)
        print('::warning::' + message, file=out)
    else:
        message = line
        print(message, file=out)
    for name, text in (('GITHUB_STEP_SUMMARY', message + '\n'),
                       ('GITHUB_OUTPUT', f"no_verdict={str(no_verdict).lower()}\npeak_load5={peak}\n")):
        target = os.environ.get(name)
        if target:
            with open(target, 'a') as handle:
                handle.write(text)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    s = sub.add_parser('start'); s.add_argument('path'); s.add_argument('--interval', type=float, default=30)
    v = sub.add_parser('verdict'); v.add_argument('path'); v.add_argument('--job-status', required=True)
    args = parser.parse_args(argv)
    if args.command == 'start':
        return start(args.path, args.interval)
    return verdict(args.path, args.job_status)


if __name__ == '__main__':
    sys.exit(main())
