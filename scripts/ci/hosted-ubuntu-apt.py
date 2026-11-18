#!/usr/bin/env python3
"""Bounded HTTPS-first provisioning without modifying the runner's apt sources."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

HELPER = Path(__file__).with_name('apt-bounded.sh')
# Match only official Ubuntu archive origins, including regional archive hosts
# such as azure.archive, us.archive and the AWS-region mirrors the hosted image
# ships since 2026-11 (us-east-2.ec2.archive). An unmatched regional host stays
# first in GitHub's mirror list and silently keeps serving every package over HTTP.
OFFICIAL = re.compile(
    r'\bhttps?://(?:(?:[a-z0-9-]+\.)*archive|security)\.ubuntu\.com(?=/ubuntu(?:/|\s|$))')
MIRROR_FILE = re.compile(r'mirror\+file:(/{1,3}[^\s#]+)')

# Time budget, in seconds. hosted-ubuntu-apt.sh stops this script after 9m (540 s);
# keep a margin for interpreter start-up and temporary-tree cleanup.
OUTER_DEADLINE = 540
BUDGET = OUTER_DEADLINE - 15
UPDATE, DOWNLOAD, INSTALL = 60, 150, 240  # nominal bound of each apt phase
# HTTPS phases are shortened so that the runner-sources route always keeps a
# full index update plus 90 s of download (the shared cache keeps earlier
# progress), and every route keeps time for the final --no-download install
# (about 20 s on the hosted runner, run 44255520770).
FALLBACK_RESERVE = UPDATE + 90
INSTALL_RESERVE = 90
MIN_PHASE = 15  # a shorter apt window cannot finish a phase; skip it instead


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


def provision(packages, source_root=Path('/etc/apt'), clock=time.monotonic, run=subprocess.run):
    deadline = clock() + BUDGET
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

        def apt(options, nominal, reserve, *args):
            # Never let one phase eat the time reserved for the routes after it.
            seconds = int(min(nominal, deadline - clock() - reserve))
            if seconds < MIN_PHASE:
                print(f'Hosted Ubuntu provisioning: skipping apt-get {args[0]}: '
                      f'{seconds}s left before the reserved time', file=sys.stderr, flush=True)
                return 124
            env = dict(os.environ, OKILUM_APT_TIMEOUT=f'{seconds}s')
            return run(['bash', str(HELPER), *common, *options, *args],
                       env=env, check=False).returncode

        def attempt(label, options, reserve):
            status = apt(options, UPDATE, reserve + INSTALL_RESERVE, 'update')
            if status == 0:
                status = apt(options, DOWNLOAD, reserve + INSTALL_RESERVE, 'install',
                             '--download-only', '-y', '--no-install-recommends', *packages)
            if status == 0:
                # Downloads are complete: the install may use all remaining time.
                return apt(options, INSTALL, 0, 'install', '--no-download', '-y',
                           '--no-install-recommends', *packages)
            print(f'Hosted Ubuntu provisioning: {label} failed (status {status})',
                  file=sys.stderr, flush=True)
            return None

        # Keep both official routes first. The independently hosted kernel.org
        # mirror also serves Ubuntu-signed indexes/packages when Ubuntu DNS is
        # unavailable; retain the original keyrings, TLS checks and outer bound.
        for index, mirror in enumerate(('azure.archive.ubuntu.com', 'archive.ubuntu.com',
                                        'mirrors.edge.kernel.org')):
            target = root / str(index)
            target.mkdir()
            options = source_copy(source_root, target, mirror)
            print(f'Hosted Ubuntu provisioning: HTTPS {mirror} (bounded update + download)', flush=True)
            status = attempt(mirror, options, FALLBACK_RESERVE)
            if status is not None:
                return status
        print('Hosted Ubuntu provisioning: all HTTPS Ubuntu routes failed', file=sys.stderr)
        # HTTPS is preferred for throughput (HTTP archive downloads stalled at
        # ~80 KB/s in October), not for integrity: apt still verifies the signed
        # Release files and package hashes over HTTP. As the last route, use the
        # runner's own sources and mirror lists unchanged, inside the reserved
        # time and with the shared cache, so completed downloads are reused.
        print('Hosted Ubuntu provisioning: runner original sources (bounded update + download)',
              flush=True)
        status = attempt('runner original sources',
                         ['-o', f'Dir::Etc::sourcelist={source_root / "sources.list"}',
                          '-o', f'Dir::Etc::sourceparts={source_root / "sources.list.d"}'],
                         0)
        if status is not None:
            return status
        print('Hosted Ubuntu provisioning: every route failed', file=sys.stderr)
        if source_root == Path('/etc/apt') and deadline - clock() >= 60:
            # Read-only diagnostics for the actual ephemeral runner; fake-apt
            # tests stay offline. Never turn transport probes into a verdict.
            for mirror in ('azure.archive.ubuntu.com', 'archive.ubuntu.com'):
                for family in ('--ipv4', '--ipv6'):
                    print(f'Hosted Ubuntu transport probe: {mirror} {family}', flush=True)
                    subprocess.run([
                        'curl', family, '--head', '--silent', '--show-error',
                        '--connect-timeout', '5', '--max-time', '10',
                        '--output', os.devnull,
                        '--write-out', 'remote_ip=%{remote_ip} http=%{http_code}\n',
                        f'https://{mirror}/ubuntu/dists/noble/InRelease',
                    ], check=False, timeout=15)
        return 1


if __name__ == '__main__':
    release = Path('/etc/os-release').read_text()
    if not re.search(r'^ID=ubuntu$', release, re.M):
        raise SystemExit('Hosted apt provisioning requires an ephemeral Ubuntu runner')
    if not sys.argv[1:]:
        raise SystemExit('Expected dependency package names')
    raise SystemExit(provision(sys.argv[1:]))
