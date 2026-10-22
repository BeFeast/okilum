#!/usr/bin/env python3
"""Bounded HTTPS provisioning without modifying the runner's apt sources."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

HELPER = Path(__file__).with_name('apt-bounded.sh')
# Match only official Ubuntu archive origins, including regional archive hosts.
OFFICIAL = re.compile(
    r'\bhttps?://(?:azure\.archive|(?:[a-z]{2}\.)?archive|security)\.ubuntu\.com(?=/ubuntu(?:/|\s|$))')
MIRROR_FILE = re.compile(r'mirror\+file:(/{1,3}[^\s#]+)')


def source_copy(source_root, target, mirror):
    """Copy .list/.sources and local mirror lists, changing official origins only."""
    mirrors = target / 'mirrors'
    mirrors.mkdir()
    copied = {}

    def rewrite(text):
        text = OFFICIAL.sub('https://' + mirror, text)

        def mirror_copy(match):
            original = Path('/' + match[1].lstrip('/'))
            if original not in copied:
                dest = mirrors / str(len(copied))
                # Local mirror lists contain URLs plus optional selection metadata.
                # Preserve third-party entries, ordering, comments and metadata.
                dest.write_bytes(OFFICIAL.sub('https://' + mirror,
                                             original.read_bytes().decode()).encode())
                copied[original] = dest
            return 'mirror+file:' + str(copied[original])

        return MIRROR_FILE.sub(mirror_copy, text)

    main = target / 'sources.list'
    original = source_root / 'sources.list'
    main.write_bytes(rewrite(original.read_bytes().decode()).encode() if original.exists() else b'')
    parts = target / 'sources.list.d'
    parts.mkdir()
    for original in sorted((source_root / 'sources.list.d').glob('*')):
        if original.is_file() and original.suffix in ('.list', '.sources'):
            (parts / original.name).write_bytes(rewrite(original.read_bytes().decode()).encode())
    return ['-o', f'Dir::Etc::sourcelist={main}', '-o', f'Dir::Etc::sourceparts={parts}']


def provision(packages, source_root=Path('/etc/apt')):
    with tempfile.TemporaryDirectory(prefix='okilum-hosted-apt-') as tmp:
        root = Path(tmp)
        # apt's _apt download user must traverse the temporary source/cache tree.
        root.chmod(0o755)
        cache = root / 'archives'
        cache.mkdir(mode=0o755)
        (cache / 'partial').mkdir(mode=0o755)
        common = ['-o', f'Dir::Cache::archives={cache}',
                  '-o', 'Acquire::https::Verify-Peer=true',
                  '-o', 'Acquire::https::Verify-Host=true']

        def apt(options, deadline, *args):
            env = dict(os.environ, OKILUM_APT_TIMEOUT=deadline)
            return subprocess.run(['bash', str(HELPER), *common, *options, *args],
                                  env=env, check=False).returncode

        for index, mirror in enumerate(('azure.archive.ubuntu.com', 'archive.ubuntu.com')):
            target = root / str(index)
            target.mkdir()
            options = source_copy(source_root, target, mirror)
            print(f'Hosted Ubuntu provisioning: HTTPS {mirror} (bounded update + download)', flush=True)
            status = apt(options, '60s', 'update')
            if status == 0:
                status = apt(options, '150s', 'install', '--download-only', '-y',
                             '--no-install-recommends', *packages)
            if status == 0:
                return apt(options, '4m', 'install', '--no-download', '-y',
                           '--no-install-recommends', *packages)
            print(f'Hosted Ubuntu provisioning: {mirror} failed (status {status})',
                  file=sys.stderr, flush=True)
        print('Hosted Ubuntu provisioning: both official HTTPS routes failed', file=sys.stderr)
        return 1


if __name__ == '__main__':
    release = Path('/etc/os-release').read_text()
    if not re.search(r'^ID=ubuntu$', release, re.M):
        raise SystemExit('Hosted apt provisioning requires an ephemeral Ubuntu runner')
    if not sys.argv[1:]:
        raise SystemExit('Expected dependency package names')
    raise SystemExit(provision(sys.argv[1:]))
