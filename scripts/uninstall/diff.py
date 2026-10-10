#!/usr/bin/env python3
"""Uninstall acceptance (#974): what exists after uninstall but not before.

    diff.py before.txt after.txt [--vault PATH ...]

Prints every added entry. Fails if an added entry belongs to Okilum (name
contains okilum / BeFeast.Okilum / com.befeast.okilum) and is not under a
vault or in the known residue owned by Windows or Velopack.
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


def entries(path):
    return {line.rstrip('\r\n') for line in Path(path).read_text(encoding='utf-8-sig').splitlines() if line.strip()}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('before')
    parser.add_argument('after')
    parser.add_argument('--vault', action='append', default=[], help='vault root that may remain')
    args = parser.parse_args()
    vaults = [v.rstrip('\\/').lower() for v in args.vault]
    added = sorted(entries(args.after) - entries(args.before))
    failures = []
    for entry in added:
        path = entry[2:]
        in_vault = any(path.lower() == v or path.lower().startswith(v + '\\') or path.lower().startswith(v + '/') for v in vaults)
        residue = any(pattern.search(path) for pattern in KNOWN_RESIDUE)
        tag = 'vault' if in_vault else 'residue' if residue else 'OKILUM' if OURS.search(path) else 'other'
        print(f'{tag:8} {entry}')
        if tag == 'OKILUM':
            failures.append(entry)
    print(f'\n{len(added)} added entries; {len(failures)} left by Okilum')
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
