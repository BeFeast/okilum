#!/usr/bin/env python3
"""Publish checksum-pinned fixture files without writing to an executing inode."""
import hashlib
import io
import os
from pathlib import Path
import sys
import tarfile
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from pinned_download import fetch


ARCHIVE = 'syncthing-linux-amd64-v2.1.6.tar.gz'
ARCHIVE_SHA256 = '524ef4e1df1850b719e2378c8369956f4e5f2ab9a46fdf7f45f2b925012147aa'
BINARY_SHA256 = 'd72663e4f9efe0b8de5e1dacb13c1c8decc82e76623cf2fa47fd306f3e1d3a3e'
MEMBER = 'syncthing-linux-amd64-v2.1.6/syncthing'
URL = f'https://github.com/syncthing/syncthing/releases/download/v2.1.6/{ARCHIVE}'
ASSET_API_URL = 'https://api.github.com/repos/syncthing/syncthing/releases/assets/614662906'


def verified(data, checksum):
    if hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError(f'fixture checksum mismatch: expected {checksum}')
    return data


def publish(destination, data, mode):
    # Every writer owns a distinct file on the destination filesystem. Close its
    # writable descriptor BEFORE rename, otherwise a concurrent exec can ETXTBSY.
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(prefix='.stage-', dir=destination.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            output.flush()
            os.fchmod(output.fileno(), mode)
            os.fsync(output.fileno())
        except BaseException:
            temporary.unlink()
            raise
    try:
        os.replace(temporary, destination)
        directory = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def stage(cache):
    cache = Path(cache).resolve()
    # A separate content-addressed path also avoids older jobs that still tar
    # directly over the legacy version-named executable directory.
    binary = cache / BINARY_SHA256 / 'syncthing'
    try:
        verified(binary.read_bytes(), BINARY_SHA256)
        if binary.stat().st_mode & 0o777 == 0o555:
            return binary
    except FileNotFoundError:
        pass
    except ValueError:
        pass
    archive = cache / ARCHIVE
    try:
        data = archive.read_bytes()
    except FileNotFoundError:
        # Each download stays private until both the pinned fetch and existing
        # archive verification pass. Publication/exec inodes remain unchanged.
        with tempfile.TemporaryDirectory(prefix='okilum-sync-download-') as temporary:
            download = Path(temporary) / ARCHIVE
            fetch(URL, ARCHIVE_SHA256, download, ASSET_API_URL)
            data = verified(download.read_bytes(), ARCHIVE_SHA256)
        publish(archive, data, 0o444)
    verified(data, ARCHIVE_SHA256)
    # Verify the exact snapshot we extract, and never extract into a live path.
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as bundle:
        member = bundle.getmember(MEMBER)
        if not member.isfile():
            raise ValueError('fixture executable must be a regular archive member')
        with bundle.extractfile(member) as source:
            executable = verified(source.read(), BINARY_SHA256)
    publish(binary, executable, 0o555)
    return binary


if __name__ == '__main__':
    cache = Path(os.environ.get('XDG_CACHE_HOME', Path.home() / '.cache')) / 'okilum-sync-fixture'
    print(stage(cache))
