#!/usr/bin/env python3
"""Prepare and verify the exact upstream Sparkle closure; never fetch signing keys."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import tarfile
import tempfile
import urllib.request

LOCK = json.loads(Path(__file__).with_name('sparkle-lock.json').read_text())
# Leaf code first, then containing framework. No --deep signing.
SIGN_ORDER = (
    'Versions/B/XPCServices/Downloader.xpc',
    'Versions/B/XPCServices/Installer.xpc',
    'Versions/B/Updater.app',
    'Versions/B/Autoupdate',
    '',
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def validate_entries(entries):
    """entries maps every non-directory framework path to (bytes, unix mode)."""
    require(set(entries) == set(LOCK['entries']), 'Unexpected or missing Sparkle closure entry')
    for name, expected in LOCK['entries'].items():
        data, mode = entries[name]
        if 'link' in expected:
            require(stat.S_ISLNK(mode) and data.decode() == expected['link'], f'Unexpected symlink: {name}')
        else:
            require(stat.S_ISREG(mode) and sha(data) == expected['sha256'], f'Changed framework file: {name}')
            require(bool(mode & 0o111) == expected['executable'], f'Changed executable mode: {name}')


def verify(directory):
    directory = Path(directory)
    framework = directory / 'Sparkle.framework'
    require(framework.is_dir() and not framework.is_symlink(), 'Missing real Sparkle framework')
    entries = {}
    for parent, dirs, files in os.walk(framework, followlinks=False):
        for name in dirs + files:
            path = Path(parent) / name
            mode = path.lstat().st_mode
            if stat.S_ISDIR(mode):
                # Empty unexpected directories also fail closed.
                rel = path.relative_to(directory).as_posix() + '/'
                require(any(n.startswith(rel) for n in LOCK['entries']), f'Unexpected directory: {rel}')
                continue
            key = path.relative_to(directory).as_posix()
            if stat.S_ISLNK(mode):
                require(path.resolve().is_relative_to(framework.resolve()), f'Escaping symlink: {key}')
                entries[key] = (os.readlink(path).encode(), mode)
            else:
                require(stat.S_ISREG(mode), f'Unexpected special file: {key}')
                entries[key] = (path.read_bytes(), mode)
    validate_entries(entries)


def prepare(archive, destination):
    archive, destination = Path(archive), Path(destination)
    require(sha(archive.read_bytes()) == LOCK['archive_sha256'], 'Sparkle archive SHA256 mismatch')
    if destination.exists():
        verify(destination)
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix='.sparkle-', dir=destination.parent))
    try:
        with tarfile.open(archive) as tar:
            members = {}
            for member in tar:
                name = member.name.removeprefix('./')
                if not name.startswith('Sparkle.framework/'):
                    continue
                require(name not in members, f'Duplicate archive path: {name}')
                require(not PurePosixPath(name).is_absolute() and '..' not in PurePosixPath(name).parts, 'Unsafe path')
                members[name] = member
            entries = {}
            for name, member in members.items():
                if member.isdir():
                    continue
                require(member.isfile() or member.issym(), 'Unsupported archive member')
                data = member.linkname.encode() if member.issym() else tar.extractfile(member).read()
                mode = (stat.S_IFLNK if member.issym() else stat.S_IFREG) | member.mode
                entries[name] = (data, mode)
            validate_entries(entries)
            # Write regular files before links, so no extraction writes through links.
            for name, (data, mode) in entries.items():
                if stat.S_ISLNK(mode):
                    continue
                path = stage / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
                path.chmod(stat.S_IMODE(mode))
            for name, (data, mode) in entries.items():
                if stat.S_ISLNK(mode):
                    path = stage / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.symlink_to(data.decode())
        verify(stage)
        stage.rename(destination)
    finally:
        if stage.exists():
            shutil.rmtree(stage)


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest='command', required=True)
    a = sub.add_parser('prepare')
    a.add_argument('--archive', type=Path, required=True)
    a.add_argument('--destination', type=Path, required=True)
    a = sub.add_parser('verify'); a.add_argument('directory', type=Path)
    a = sub.add_parser('fetch'); a.add_argument('archive', type=Path)
    args = p.parse_args()
    if args.command == 'fetch':
        # Exact upstream URL and digest; a failed fetch never creates the destination.
        with urllib.request.urlopen(LOCK['url'], timeout=60) as response:
            data = response.read()
        require(sha(data) == LOCK['archive_sha256'], 'Sparkle download SHA256 mismatch')
        with args.archive.open('xb') as output:
            output.write(data)
    elif args.command == 'prepare':
        prepare(args.archive, args.destination)
    else:
        verify(args.directory)
