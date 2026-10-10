#!/usr/bin/env python3
"""Uninstall acceptance (#974): what exists after uninstall but not before.

    diff.py before.txt after.txt [--vault PATH ...] [--home PATH]

Prints every added entry. Fails if an added entry belongs to Okilum (name
contains okilum / BeFeast.Okilum / com.befeast.okilum) and is not under a
vault, the exported unsaved drafts, or the known residue owned by Windows or
Velopack. Only the part of a path below the snapshot's home is checked for the
name, so a QA home such as /home/qa/okilum-night/home is not app residue (#1088).
The home comes from the snapshot's "# home" line or --home.
"""
import argparse
import re
import sys
from pathlib import Path

OURS = re.compile(r'okilum', re.IGNORECASE)
# Owned by Windows or Velopack, documented in docs/uninstall.md.
KNOWN_RESIDUE = [
    re.compile(r'\\AppData\\Local\\velopack(\\velopack\.log)?$', re.IGNORECASE),
    re.compile(r'\\Explorer\\.*MuiCache', re.IGNORECASE),
    re.compile(r'\\AppData\\Roaming\\Microsoft\\Windows\\Recent\\', re.IGNORECASE),
]
# Uninstall deliberately exports unsaved drafts here (docs/uninstall.md).
EXPORT = re.compile(r'[\\/]Documents[\\/]Okilum unsaved drafts([\\/]|$)')


def entries(path):
    """Snapshot entries and the home recorded in its "# home" header, if any."""
    lines = Path(path).read_text(encoding='utf-8-sig').splitlines()
    home = next((line[len('# home '):].strip() for line in lines if line.startswith('# home ')), None)
    return {line.rstrip('\r\n') for line in lines if line.strip() and not line.startswith('#')}, home


def below(path, root):
    """The part of path under root (either separator, any case), or path itself."""
    if root:
        root = root.rstrip('\\/')
        lowered = path.lower()
        for sep in ('\\', '/'):
            if lowered.startswith(root.lower() + sep):
                return path[len(root) + 1:]
    return path


def classify(path, vaults, home):
    lowered = path.lower()
    if any(lowered == v or lowered.startswith(v + '\\') or lowered.startswith(v + '/') for v in vaults):
        return 'vault'
    if any(pattern.search(path) for pattern in KNOWN_RESIDUE):
        return 'residue'
    if EXPORT.search(path):
        return 'export'
    return 'OKILUM' if OURS.search(below(path, home)) else 'other'


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('before')
    parser.add_argument('after')
    parser.add_argument('--vault', action='append', default=[], help='vault root that may remain')
    parser.add_argument('--home', help='snapshot home; defaults to the snapshot header')
    args = parser.parse_args()
    vaults = [v.rstrip('\\/').lower() for v in args.vault]
    before, home_before = entries(args.before)
    after, home_after = entries(args.after)
    home = args.home or home_after or home_before
    added = sorted(after - before)
    failures = []
    for entry in added:
        tag = classify(entry[2:], vaults, home)
        print(f'{tag:8} {entry}')
        if tag == 'OKILUM':
            failures.append(entry)
    print(f'\n{len(added)} added entries; {len(failures)} left by Okilum')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
