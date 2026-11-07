#!/usr/bin/env python3
"""Read only: compare selected Mac vault roots with their existing writer bindings.

No mkdir, registration, rebind, lock acquisition, data reads, subprocess or GUI.
Output contains only selected root paths and identity/phase metadata, no tokens.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vault', action='append', required=True)
    args = parser.parse_args()
    if sys.platform != 'darwin':
        parser.error('Run this read-only collector on the affected Mac')
    account = Path.home() / 'Library/Application Support/com.befeast.okilum/local-writers-v1'
    report = {'platform': sys.platform, 'account_exists': account.is_dir(), 'vaults': [], 'bindings': []}
    visited = set()
    for value in args.vault:
        vault = Path(value).expanduser()
        if not vault.is_absolute():
            parser.error('--vault must be absolute')
        metadata = vault.lstat()
        report['vaults'].append({'path': str(vault), 'dev': metadata.st_dev, 'ino': metadata.st_ino,
                                'birth_ns': getattr(metadata, 'st_birthtime_ns', None),
                                'birth_seconds': getattr(metadata, 'st_birthtime', None),
                                'is_symlink': vault.is_symlink()})
        for root in (vault, *vault.parents):
            if root in visited:
                continue
            visited.add(root)
            key = hashlib.sha256(os.fsencode(root)).hexdigest()
            path = account / (key + '.json')
            if not path.is_file():
                continue
            if path.stat().st_size > 65536:
                raise RuntimeError('Oversized binding; no content read')
            binding = json.loads(path.read_text())
            metadata = root.lstat()
            stored = binding.get('root_identity')
            current_birth = getattr(metadata, 'st_birthtime_ns', None)
            entry = {'root': str(root), 'binding_root_matches': binding.get('root') == str(root),
                     'stored_identity': stored, 'current_dev': metadata.st_dev,
                     'current_ino': metadata.st_ino, 'current_birth_ns': current_birth,
                     'current_birth_seconds': getattr(metadata, 'st_birthtime', None)}
            if isinstance(stored, list) and len(stored) >= 2:
                entry['same_dev_inode'] = stored[:2] == [metadata.st_dev, metadata.st_ino]
                if len(stored) == 3 and current_birth is not None:
                    entry['same_birth_ns'] = stored[2] == current_birth
            state = account / key / 'state.json'
            if state.is_file() and state.stat().st_size <= 65536:
                state_data = json.loads(state.read_text())
                entry['phase'] = state_data.get('phase')
                entry['revision'] = state_data.get('revision')
            report['bindings'].append(entry)
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
