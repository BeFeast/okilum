#!/usr/bin/env python3
"""Run one hosted gate with source/run/attempt-bound, exit-bearing log markers."""
import argparse
import json
import os
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--name', required=True)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command or not re.fullmatch('[0-9a-f]{40}', os.environ.get('GITHUB_SHA', '')):
        parser.error('Command and exact source SHA are required')
    identity = {'step': args.name, 'sha': os.environ['GITHUB_SHA'], 'job': os.environ['GITHUB_JOB'],
                'run': int(os.environ['GITHUB_RUN_ID']), 'attempt': int(os.environ['GITHUB_RUN_ATTEMPT'])}
    if min(identity['run'], identity['attempt']) <= 0:
        parser.error('Positive run and attempt required')
    print('OKILUM_GATE_V1 BEGIN ' + json.dumps(identity, sort_keys=True), flush=True)
    try:
        result = subprocess.run(command).returncode
    except OSError:
        result = 127
    print('OKILUM_GATE_V1 END ' + json.dumps(dict(identity, exit=result), sort_keys=True), flush=True)
    return result if result >= 0 else 128 - result


if __name__ == '__main__':
    raise SystemExit(main())
