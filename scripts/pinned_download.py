#!/usr/bin/env python3
"""Download pinned build bytes, falling back to the public GitHub asset API."""
import argparse
import hashlib
from pathlib import Path
import urllib.error
import urllib.request


def fetch(url, digest, destination, fallback=None):
    destination = Path(destination)
    if destination.exists():
        raise FileExistsError(destination)
    try:
        request = urllib.request.Request(url, headers={'User-Agent': 'okilum-pinned-download'})
        with urllib.request.urlopen(request, timeout=60) as response:
            data = response.read()
    except (urllib.error.URLError, TimeoutError) as error:
        if not fallback:
            raise
        print(f'Primary artifact download failed ({error}); trying pinned GitHub asset API', flush=True)
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        request = urllib.request.Request(fallback, headers={
            'Accept': 'application/octet-stream', 'User-Agent': 'okilum-pinned-download'})
        with urllib.request.urlopen(request, timeout=60) as response:
            data = response.read()
    # Both routes must supply the same locked bytes. A digest failure is never
    # retried through another route or published as an existing destination.
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError('Pinned artifact SHA256 mismatch')
    with destination.open('xb') as output:
        output.write(data)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--fallback')
    args = parser.parse_args()
    fetch(args.url, args.sha256, args.destination, args.fallback)
