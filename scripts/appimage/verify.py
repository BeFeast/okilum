"""Fail closed on missing ELF dependencies and mismatched AppImage provenance."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def require_loaded(output, returncode):
    if returncode or re.search(r'not found|version .* not found', output):
        raise ValueError('ELF dependency load failed: ' + output)


def verify(artifact, extracted):
    provenance = json.loads((artifact / 'provenance.json').read_text())
    image = artifact / provenance['artifact']
    if hashlib.sha256(image.read_bytes()).hexdigest() != provenance['sha256']:
        raise ValueError('AppImage checksum differs from provenance')
    if (extracted / 'SOURCE_SHA').read_text().strip() != provenance['source_sha']:
        raise ValueError('Extracted source identity differs from provenance')
    # ldd needs a positive control too: same parser must reject missing libraries.
    try:
        require_loaded('libpositive-control.so => not found', 0)
    except ValueError:
        pass
    else:
        raise ValueError('Missing-library detector failed its positive control')
    # Only the dependency audit gets this path; native launch must establish its own.
    loader_env = dict(os.environ, LD_LIBRARY_PATH=str(extracted / 'usr/lib'))
    result = subprocess.run(['ldd', str(extracted / 'usr/bin/okilum')],
                            capture_output=True, text=True, env=loader_env)
    print(result.stdout + result.stderr)
    require_loaded(result.stdout + result.stderr, result.returncode)
    for path in ('okilum.desktop', 'okilum.svg', 'AppRun'):
        if not (extracted / path).exists():
            raise ValueError('Missing AppDir entry: ' + path)


if __name__ == '__main__':
    verify(Path(sys.argv[1]), Path(sys.argv[2]))
