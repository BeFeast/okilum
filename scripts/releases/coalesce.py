#!/usr/bin/env python3
"""Select hourly release work without occupying a compiler runner while idle."""
import argparse
import json
import os
from pathlib import Path
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


def descriptor(source, platform):
    url = f'https://updates.befeast.com/tessera/releases/{source}/{platform}.json'
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


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['macos', 'windows'])
    args = parser.parse_args()
    event, source = os.environ['GITHUB_EVENT_NAME'], os.environ['GITHUB_SHA']
    published = descriptor(source, args.platform) if event == 'schedule' else None
    build = needed(event, published, source, args.platform)
    with Path(os.environ['GITHUB_OUTPUT']).open('a') as output:
        output.write(f'build={str(build).lower()}\n')
    print('Build newest commit' if build else 'No build: coalescing or already published')
