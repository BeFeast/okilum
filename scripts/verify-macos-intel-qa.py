#!/usr/bin/env python3
"""Reject incompatible deployment targets in the Intel QA bundle."""
import plistlib
import re
import subprocess
import sys
from pathlib import Path


def verify(app):
    info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
    assert info['LSMinimumSystemVersion'] == '12.0'
    assert 'SUFeedURL' not in info, 'QA bundle must not use the arm64 feed'
    inspected = set()
    for path in (app / 'Contents').rglob('*'):
        if not path.is_file() or path.is_symlink():
            continue
        resolved = path.resolve()
        if resolved in inspected:
            continue
        description = subprocess.check_output(['file', '-b', str(path)], text=True)
        if 'Mach-O' not in description:
            continue
        inspected.add(resolved)
        architectures = subprocess.check_output(['lipo', '-archs', str(path)], text=True).split()
        assert 'x86_64' in architectures, f'Missing Intel slice: {path}'
        commands = subprocess.check_output(['otool', '-arch', 'x86_64', '-l', str(path)], text=True)
        versions = re.findall(r'\bminos\s+([0-9.]+)', commands)
        versions += re.findall(r'cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version ([0-9.]+)', commands)
        assert versions, f'Missing deployment target: {path}'
        for version in versions:
            assert tuple((list(map(int, version.split('.'))) + [0, 0])[:3]) <= (12, 0, 0), (path, version)
        print(f'Intel macOS minimum {versions}: {path.name}')
    assert (app / 'Contents/MacOS/okilum').resolve() in inspected


if __name__ == '__main__':
    verify(Path(sys.argv[1]))
