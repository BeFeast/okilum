#!/usr/bin/env python3
"""Select periodic release work without occupying a compiler runner while idle."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess


def needed(event, published, source, platform):
    if event == 'push':
        # Defensive no-op: secondary release workflows have no push trigger.
        return False
    if event in ('workflow_dispatch', 'pull_request'):
        return True
    if event != 'schedule':
        raise ValueError('Unexpected release event')
    if published is None:
        return True
    if published['source'] != source or published['platform'] != platform:
        raise ValueError('Invalid publication descriptor')
    return False


def main_tip():
    result = subprocess.run(['git', 'ls-remote', '--exit-code', 'origin',
                             'refs/heads/main'], check=True, capture_output=True,
                            text=True, timeout=15)
    fields = result.stdout.split()
    if (len(fields) != 2 or fields[1] != 'refs/heads/main'
            or len(fields[0]) != 40
            or any(c not in '0123456789abcdef' for c in fields[0])):
        raise ValueError('Cannot identify current main for scheduled build')
    return fields[0]


def scheduled_source(event, source, platform):
    """The commit an automatic Windows/Linux tick builds: main as it is now.

    A tick carries the main snapshot from when it fired. Merges land faster than the
    tick reaches its runner, so skipping an outdated snapshot starved beta for hours;
    the tick builds the current main tip instead (unless that is already published).
    """
    if event != 'schedule' or platform not in ('windows', 'linux'):
        return source
    return main_tip()


def descriptor(source, platform):
    url = f'https://updates.befeast.com/okilum/releases/{source}/{platform}.json'
    # curl uses the same public endpoint and transport as installed update clients.
    result = subprocess.run(['curl', '--silent', '--show-error', '--location',
                             '--max-time', '30', '--write-out', '\n%{http_code}', url],
                            check=True, capture_output=True, text=True)
    body, status = result.stdout.rsplit('\n', 1)
    if status == '404':
        return None
    if status != '200':
        raise ValueError(f'Cannot check published release: HTTP {status}')
    return json.loads(body)


def explicit_source(value, platform, ref, event):
    """A requested older main commit (#1040): built once, published to the archive only.

    Returns None when no source was requested. Refuses anything that is not a full
    SHA on main's history: the checkout is main with full history, so a commit that
    only exists on a branch is unknown here or not an ancestor of HEAD.
    """
    value = (value or '').strip()
    if not value:
        return None
    if platform == 'macos':
        raise ValueError('An explicit source is not available for the macOS lane yet')
    if event != 'workflow_dispatch' or ref != 'refs/heads/main':
        raise ValueError('An explicit source is built only by a manual run on main')
    if not re.fullmatch('[0-9a-f]{40}', value):
        raise ValueError('The source must be a full 40-character commit SHA')
    ancestor = subprocess.run(['git', 'merge-base', '--is-ancestor', value, 'HEAD'],
                              capture_output=True, text=True, timeout=30)
    if ancestor.returncode != 0:
        raise ValueError(f'{value} is not a commit on main')
    return value


def check_resign(resign, requested, platform):
    """Re-sign (#1104): the bytes of the published Windows build of this commit, signed and
    published under a new build number; nothing is compiled. Only that exact build qualifies."""
    if platform != 'windows' or not requested or not resign.isdigit():
        raise ValueError('Re-signing needs the Windows lane, an explicit source and a build number')
    published = descriptor(requested, 'windows')
    if published is None or published['build'] != int(resign):
        raise ValueError(f'Build {resign} is not the published Windows build of {requested}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['macos', 'windows', 'linux'])
    args = parser.parse_args()
    event, source = os.environ['GITHUB_EVENT_NAME'], os.environ['GITHUB_SHA']
    requested = explicit_source(os.environ.get('OKILUM_SOURCE'), args.platform,
                                os.environ.get('GITHUB_REF', ''), event)
    resign = (os.environ.get('OKILUM_RESIGN_BUILD') or '').strip()
    if resign:
        check_resign(resign, requested, args.platform)
        build, source = True, requested
        print(f'Re-sign Windows build {resign} of {requested}')
    elif requested:
        # An older main commit: build it unless its platform build already exists.
        build = descriptor(requested, args.platform) is None
        source = requested
        print(f'Explicit source {requested}: ' + ('build' if build else 'already published'))
    else:
        tip = scheduled_source(event, source, args.platform)
        if tip != source:
            print(f'Scheduled snapshot {source[:8]} is behind main; building main tip {tip[:8]}')
            source = tip
        published = descriptor(source, args.platform) if event == 'schedule' else None
        build = needed(event, published, source, args.platform)
        print('Build newest commit' if build else 'No build: coalescing or already published')
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'build={str(build).lower()}\n')
        output.write(f'source={source}\n')
        output.write(f'resign={resign}\n')
