"""Describe the exact reusable AppImage, without claiming native acceptance."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

artifact = Path('Okilum-x86_64.AppImage')
Path('provenance.json').write_text(json.dumps({
    'source_sha': os.environ['GITHUB_SHA'],
    'build_version': os.environ['OKILUM_BUILD_VERSION'],
    'release_version': os.environ['OKILUM_RELEASE_VERSION'],
    'workflow_run': os.environ['GITHUB_RUN_ID'],
    'workflow_attempt': os.environ['GITHUB_RUN_ATTEMPT'],
    'build_base': 'ubuntu:22.04',
    'rustc': subprocess.check_output(['rustc', '-Vv'], text=True),
    'artifact': artifact.name,
    'sha256': hashlib.sha256(artifact.read_bytes()).hexdigest(),
    'tool_sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                    for p in sorted(Path('tools').iterdir()) if p.is_file()},
    'bundled_libraries': Path('bundled-libraries.txt').read_text().splitlines(),
    'signed': False,
    'acceptance_harness': os.environ.get('GITHUB_REF_NAME', '').startswith('forgejo-linux-pr/'),
}, indent=2) + '\n')
