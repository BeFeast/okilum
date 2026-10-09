#!/usr/bin/env python3
"""Hand off completed build artifacts to a non-cancellable publication workflow."""
import argparse
import io
import json
import re
import xml.etree.ElementTree as ET
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
from release import Forgejo, R2

SCRIPTS = Path(__file__).resolve().parents[1]
WORKFLOWS = {'macos': 'macos-release.yml', 'linux': 'linux-release.yml',
             'windows': 'windows-diagnostic.yml'}
ARTIFACTS = {'macos': {'macos-publication': 'macos'},
             'linux': {'arch-publication': 'arch'},
             'windows': {'okilum-windows-velopack': 'windows', 'windows-portable': 'portable'}}


def eligible(run, platform, head):
    if (run['workflow_id'] != WORKFLOWS[platform] or run['prettyref'] != 'main'
            or run['is_fork_pull_request'] or run['trigger_event'] not in ['push', 'workflow_dispatch', 'schedule']):
        raise ValueError('Publication requires a trusted main release build')
    return run['status'] == 'success' and (snapshot_run(run, platform) or run['commit_sha'] == head)


def snapshot_run(run, platform):
    return platform == 'linux' or run['trigger_event'] in ['schedule', 'workflow_dispatch']


def published_build(store, platform):
    keys = {'linux': 'okilum/arch/beta/x86_64/latest.json',
            'windows': 'okilum/windows/beta/releases.beta.json',
            'macos': 'okilum/appcast.xml'}
    current = store.call('GET', keys[platform])
    if current is None:
        return 0
    if platform == 'linux':
        return int(json.loads(current)['build'])
    if platform == 'windows':
        versions = [a['Version'] for a in json.loads(current)['Assets']]
        if not versions or any(not re.fullmatch(r'0\.1\.[0-9]+', v) for v in versions):
            raise ValueError('Unexpected Windows feed version')
        return max(int(v.split('.')[2]) for v in versions)
    channel = ET.fromstring(current).find('channel')
    if channel is None:
        raise ValueError('Invalid macOS appcast')
    # Sparkle accepts stable on every channel; match its existing all-item guard.
    version = '{http://www.andymatuschak.org/xml-namespaces/sparkle}version'
    return max((int(item.findtext(version)) for item in channel.findall('item')), default=0)


def extract(data, directory):
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for entry in archive.infolist():
            path = Path(entry.filename)
            if path.is_absolute() or '..' in path.parts or (entry.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError('Unsafe build artifact path')
        archive.extractall(directory)


def one(root, pattern):
    files = list(root.glob(pattern))
    if len(files) != 1:
        raise ValueError(f'Expected one {pattern} artifact')
    return str(files[0])


def publish(client, platform, run_id):
    # Dispatch is the build's final step. Wait only for its final success record;
    # a newer push cancelling it must never authorize publication.
    for _ in range(120):
        run = client.call('GET', f'/actions/runs/{run_id}')
        head = client.call('GET', '/branches/main')['commit']['id']
        if (run['status'] in ['failure', 'cancelled', 'skipped']
                or (not snapshot_run(run, platform) and run['commit_sha'] != head)):
            print('Cancelled, failed or superseded build: nothing published')
            return
        if eligible(run, platform, head):
            break
        time.sleep(5)
    else:
        raise ValueError('Source build did not finish successfully')
    build = 5000 + run['index_in_repo']
    # Serialized with every platform publisher and stable promotion. Complete
    # trusted snapshots remain useful after a merge, but never roll back a feed.
    if published_build(R2(), platform) > build:
        print(f'Newer {platform} build already published: nothing changed')
        return
    artifacts = client.call('GET', f'/actions/runs/{run_id}/artifacts')
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for name, folder in ARTIFACTS[platform].items():
            matching = [a for a in artifacts if a['name'] == name and not a['expired'] and a['run_id'] == run_id]
            if len(matching) != 1:
                raise ValueError(f'Missing or ambiguous artifact: {name}')
            data = client.call('GET', f'/actions/artifacts/{matching[0]["id"]}/zip', raw=True)
            extract(data, root / folder)
        # Recheck after downloads; no public mutation precedes this check.
        if (client.call('GET', '/branches/main')['commit']['id'] != run['commit_sha']
                and not snapshot_run(run, platform)):
            print('Superseded during artifact transfer: nothing published')
            return
        env = {**os.environ, 'GITHUB_SHA': run['commit_sha'],
               'GITHUB_RUN_NUMBER': str(run['index_in_repo'])}
        if platform == 'macos':
            command = [str(SCRIPTS / 'releases/macos-artifact.py'), 'publish', str(root / 'macos')]
        elif platform == 'linux':
            command = [str(SCRIPTS / 'arch/publish.py'), 'publish', '--build', str(build),
                       '--source', run['commit_sha'], '--package', one(root / 'arch', '*.pkg.tar.zst')]
        else:
            command = [str(SCRIPTS / 'windows/publish.py'), 'publish', '--build', str(build),
                       '--source', run['commit_sha'], '--directory', str(root / 'windows'),
                       '--portable', one(root / 'portable', '*.zip')]
        subprocess.run([sys.executable, *command], env=env, check=True)
        print(f'Published {platform} build {build} from completed run {run_id}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['dispatch', 'publish'])
    parser.add_argument('platform', choices=WORKFLOWS)
    parser.add_argument('--run', type=int)
    args = parser.parse_args()
    client = Forgejo()
    if args.command == 'dispatch':
        client.call('POST', '/actions/workflows/release-publish.yml/dispatches',
                    {'ref': 'main', 'inputs': {'platform': args.platform, 'run_id': os.environ['GITHUB_RUN_ID']}})
    else:
        if args.run is None or args.run < 1:
            parser.error('--run must identify the source build')
        publish(client, args.platform, args.run)
