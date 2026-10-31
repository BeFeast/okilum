"""Validate the hosted signed payload before Forgejo-only Sparkle signing."""
import hashlib
import io
import json
from pathlib import Path
import re
import zipfile


def stage(data, item, source, workflow, build, producer, github_run, destination):
    if 'sha256:' + hashlib.sha256(data).hexdigest() != item['digest']:
        raise ValueError('Hosted Mac archive digest mismatch')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive.namelist()
        if len(names) != 4 or len(set(names)) != 4 or any(Path(n).name != n for n in names):
            raise ValueError('Unexpected hosted Mac archive members')
        files = {n: archive.read(n) for n in names}
    provenance = json.loads(files['provenance.json'])
    metadata = json.loads(files['release.json'])
    expected = {'source': source, 'workflow_sha': workflow, 'build': int(build),
                'producer': str(producer), 'producer_attempt': '1', 'github_run': str(github_run)}
    if any(provenance.get(k) != v for k, v in expected.items()):
        raise ValueError('Hosted Mac producer identity mismatch')
    if (metadata.get('SOURCE_SHA') != source or metadata.get('BUILD') != str(build)
            or metadata.get('DISPLAY_VERSION') != '0.1.' + str(build)
            or metadata.get('SOURCE_TREE') != provenance['tree']
            or not re.fullmatch('[0-9a-f]{40}', provenance['tree'])):
        raise ValueError('Hosted Mac metadata identity mismatch')
    assets = provenance['assets']
    if len(assets) != 2 or {a['name'] for a in assets} != {metadata['archive'], metadata['dmg']}:
        raise ValueError('Hosted Mac payload list mismatch')
    for asset in assets:
        raw = files[asset['name']]
        if len(raw) != asset['size'] or hashlib.sha256(raw).hexdigest() != asset['sha256']:
            raise ValueError('Hosted Mac payload checksum mismatch')
    if set(files) != {'release.json', 'provenance.json', metadata['archive'], metadata['dmg']}:
        raise ValueError('Unexpected hosted Mac payload')
    root = Path(destination)
    if root.exists():
        raise ValueError('Mac publication staging directory already exists')
    root.mkdir(parents=True)
    for name, raw in files.items():
        (root / name).write_bytes(raw)
    return metadata
