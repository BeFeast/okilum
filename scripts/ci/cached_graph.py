#!/usr/bin/env python3
"""Shared manifest inventory for cached validation packets (#1815)."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil


def graph_paths(root):
    root = Path(root)
    # Root and vendored locks are required, including when unchanged by the PR.
    names = {'Cargo.toml', 'Cargo.lock', 'vendor/gpui-component/Cargo.lock'}
    names.update(p.relative_to(root).as_posix() for p in (root / 'crates').rglob('Cargo.toml')
                 if 'target' not in p.relative_to(root / 'crates').parts)
    return sorted(names)


def save(root, out, contract):
    root, out = Path(root), Path(out)
    graph = graph_paths(root)
    names = set(contract['files']) | set(graph)
    names.update(p.relative_to(root).as_posix() for p in (root / 'vendor/gpui-component').rglob('*.rs')
                 if 'target' not in p.relative_to(root / 'vendor/gpui-component').parts)
    # Validate the complete inventory before creating a partial receipt.
    for name in names:
        path = Path(name)
        if path.is_absolute() or '..' in path.parts or not (root / path).is_file():
            raise ValueError(f'Invalid or missing protected input: {name}')
    for name, expected in contract['files'].items():
        if digest(root / name) != expected['sha256']:
            raise ValueError(f'Contract hash mismatch: {name}')
    (out / 'originals').mkdir(parents=True, exist_ok=False)
    for name in sorted(names):
        destination = out / 'originals' / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / name, destination)
    (out / 'protected.json').write_text(json.dumps(sorted(names), indent=2) + '\n')
    (out / 'graph.json').write_text(json.dumps(graph, indent=2) + '\n')


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def verify(root, out):
    root, out = Path(root), Path(out)
    names = json.loads((out / 'graph.json').read_text())
    if names != graph_paths(root):
        raise ValueError('Manifest inventory changed after snapshot')
    for name in names:
        if digest(root / name) != digest(out / 'originals' / name):
            raise ValueError(f'Graph input changed: {name}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['save', 'graph'])
    parser.add_argument('--checkout', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if args.command == 'save':
        contract = json.loads((args.out / 'TEST-CONTRACT.json').read_text())
        save(args.checkout, args.out, contract)
    else:
        verify(args.checkout, args.out)


if __name__ == '__main__':
    main()
