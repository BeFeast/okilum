#!/usr/bin/env python3
"""Transfer a notarized, Sparkle-signed archive between build and publication jobs."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sys
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
from release import publish


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['stage', 'publish'])
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    root = args.directory
    if args.command == 'stage':
        root.mkdir(parents=True, exist_ok=True)
        archive = Path(os.environ['ARCHIVE'])
        shutil.copyfile(archive, root / archive.name)
        metadata = {key: os.environ[key] for key in
                    ['BUILD', 'DISPLAY_VERSION', 'SOURCE_SHA', 'SOURCE_TREE', 'SIGNATURE']}
        metadata['archive'] = archive.name
        (root / 'release.json').write_text(json.dumps(metadata))
    else:
        metadata = json.loads((root / 'release.json').read_text())
        if (metadata['SOURCE_SHA'] != os.environ['GITHUB_SHA']
                or int(metadata['BUILD']) != 5000 + int(os.environ['GITHUB_RUN_NUMBER'])
                or Path(metadata['archive']).name != metadata['archive']):
            raise ValueError('Release artifact does not belong to this run')
        publish(SimpleNamespace(app='tessera', archive=str(root / metadata['archive']),
                build=int(metadata['BUILD']), short_version=metadata['DISPLAY_VERSION'],
                source=metadata['SOURCE_SHA'], tree=metadata['SOURCE_TREE'],
                signature=metadata['SIGNATURE'], channel='beta'))


if __name__ == '__main__':
    main()
