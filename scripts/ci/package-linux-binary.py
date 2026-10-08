#!/usr/bin/env python3
"""Package an unsigned QA binary with verifiable source and file identities."""
import argparse
import hashlib
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def package(binary, output, source_sha):
    if not re.fullmatch(r'[0-9a-f]{40}', source_sha):
        raise ValueError('Expected a full source commit SHA')
    if not binary.is_file() or binary.is_symlink():
        raise ValueError('Expected a regular compiled binary')
    output.mkdir(parents=True, exist_ok=True)
    name = f'tessera-linux-x86_64-{source_sha}'
    archive = output / (name + '.tar.zst')
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary) / name
        root.mkdir()
        shutil.copyfile(binary, root / 'tessera')
        (root / 'tessera').chmod(0o755)
        (root / 'SOURCE_SHA').write_text(source_sha + '\n')
        for notice in ('LICENSE', 'THIRD_PARTY_NOTICES.md'):
            shutil.copyfile(Path(__file__).resolve().parents[2] / notice, root / notice)
        entries = sorted(root.iterdir())
        (root / 'SHA256SUMS').write_text(''.join(
            hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.name + '\n'
            for path in entries))
        subprocess.run(['tar', '--zstd', '-cf', str(archive.resolve()), '-C', temporary, name], check=True)
    archive.with_suffix(archive.suffix + '.sha256').write_text(
        hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
    return archive


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--source-sha', required=True)
    args = parser.parse_args()
    print(package(args.binary, args.output, args.source_sha))
