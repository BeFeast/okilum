#!/usr/bin/env python3
"""Report-only readout of the nightly arch-live RSS probe (#2037, Oleg 17.11).

Usage: rss-probe-report.py PROBE_LOG LABEL [SUMMARY]

Reads the output of scripts/check-edit-memory-native.py and writes a verdict
to the job summary (SUMMARY, else $GITHUB_STEP_SUMMARY). A failed check is an
error annotation, a run that proves nothing (or no output) a warning. Always
exits 0: the probe informs and never gates anything.
"""
import ast
import os
import re
import sys
from pathlib import Path

TITLE = 'arch-live RSS no_growth probe (report-only)'


def readout(text, label):
    """(verdict, result, checks, reason) from the probe's log."""
    result = checks = None
    cycles = 0
    for line in text.splitlines():
        if not line.startswith(label + ' '):
            continue
        rest = line[len(label) + 1:]
        if rest.startswith('INVALID:'):
            return 'INVALID', None, None, rest[len('INVALID:'):].strip()
        if re.match(r'cycle \d+ rss_kib ', rest):
            cycles += 1
        elif rest.startswith('{'):
            try:
                value = ast.literal_eval(rest)
            except (ValueError, SyntaxError):
                continue
            if isinstance(value, dict) and value and all(isinstance(v, bool) for v in value.values()):
                checks = value
            elif isinstance(value, dict):
                result = value
    if checks is None:
        return 'NO_RESULT', result, None, f'no verdict in the probe output ({cycles} cycles logged)'
    failed = sorted(name for name, ok in checks.items() if not ok)
    if failed:
        return 'FAIL', result, checks, 'failed: ' + ', '.join(failed)
    return 'PASS', result, checks, f'{cycles} cycles'


def markdown(verdict, result, checks, reason):
    lines = [f'### {TITLE}: **{verdict}**', '', reason, '']
    if result:
        lines += ['| reading | value |', '|---|---|']
        lines += [f'| `{key}` | {value} |' for key, value in result.items()]
        lines.append('')
    if checks:
        lines += ['| check | holds |', '|---|---|']
        lines += [f'| `{key}` | {"yes" if ok else "**no**"} |' for key, ok in checks.items()]
        lines.append('')
    lines.append('Limits: level under 20 MiB, slope under 64 KiB/cycle (Theil-Sen over the restored '
                 'cycles after warm-up), note restored every cycle. Nothing depends on this job.')
    return '\n'.join(lines) + '\n'


def annotation(verdict, reason):
    if verdict == 'PASS':
        return None
    kind = 'error' if verdict == 'FAIL' else 'warning'
    return f'::{kind} title={TITLE}::{verdict}: {reason}'


def main(argv):
    if len(argv) not in (3, 4):
        print(__doc__, file=sys.stderr)
        return 0
    log, label = Path(argv[1]), argv[2]
    summary = argv[3] if len(argv) == 4 else os.environ.get('GITHUB_STEP_SUMMARY')
    try:
        text = log.read_text(errors='replace')
    except OSError as error:
        text = ''
        verdict, result, checks, reason = 'NO_RESULT', None, None, f'no probe log: {error}'
    else:
        verdict, result, checks, reason = readout(text, label)
    report = markdown(verdict, result, checks, reason)
    print(report, end='')
    note = annotation(verdict, reason)
    if note:
        print(note)
    if summary:
        with open(summary, 'a') as stream:
            stream.write(report)
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
