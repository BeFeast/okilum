"""Offline admission of original hosted PR artifact bytes, never repackaging."""
import hashlib
import io
import json
from pathlib import Path
import re
import xml.etree.ElementTree as ET
import zipfile

LIMITATIONS = 'unsigned PR seed; Stable-to-PR deferred to pre-Stable gate; system proxy requires Ivan; #1667 open'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def load_pr_seed(path):
    m = json.loads(path.read_text())
    source = m['source']
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Invalid exact source')
    metadata = json.loads(Path(m['artifact_metadata']).read_text())
    if (metadata['workflow_run']['head_sha'] != source or metadata['expired']
            or metadata['name'] != 'unsigned-windows'):
        raise ValueError('Artifact source/name/expiry mismatch')
    run = json.loads(Path(m['hosted_run']).read_text())
    if (run['head_sha'] != source or run['id'] != metadata['workflow_run']['id']
            or run['status'] != 'completed' or run['conclusion'] != 'success'
            or run['path'] != '.github/workflows/forgejo-windows-release.yml'):
        raise ValueError('Exact-source successful packaging run not proven')
    archive = Path(m['artifact_zip']).read_bytes()
    if metadata['digest'] != 'sha256:' + digest(archive):
        raise ValueError('Original CI artifact archive digest mismatch')
    with zipfile.ZipFile(io.BytesIO(archive)) as z:
        names = z.namelist()
        if len(names) != len(set(names)):
            raise ValueError('Duplicate artifact members')
        setup_name = 'BeFeast.Okilum-beta-Setup.exe'
        packages = [n for n in names if re.fullmatch(r'BeFeast.Okilum-0\.1\.[0-9]+-beta-full\.nupkg', n)]
        if len(packages) != 1 or names.count(setup_name) != 1:
            raise ValueError('Ambiguous Setup/package')
        package_name = packages[0]
        setup, package = z.read(setup_name), z.read(package_name)
    if Path(m['setup']).read_bytes() != setup:
        raise ValueError('Setup differs from original CI archive')
    with zipfile.ZipFile(io.BytesIO(package)) as z:
        nuspecs = [n for n in z.namelist() if n.endswith('.nuspec')]
        if len(nuspecs) != 1:
            raise ValueError('Ambiguous package metadata')
        root = ET.fromstring(z.read(nuspecs[0]))
        def value(tag):
            found = [e.text for e in root.iter() if e.tag.split('}')[-1] == tag]
            if len(found) != 1:
                raise ValueError('Ambiguous package ' + tag)
            return found[0]
        version = value('version')
        if value('id') != 'BeFeast.Okilum' or not re.fullmatch(r'0\.1\.[0-9]+', version):
            raise ValueError('Wrong package identity')
        if package_name != f'BeFeast.Okilum-{version}-beta-full.nupkg':
            raise ValueError('Filename/version mismatch')
        if 'lib/app/okilum.exe' not in z.namelist():
            raise ValueError('App absent')
    return dict(source=source, setup=str(Path(m['setup']).resolve()), limitations=LIMITATIONS,
                baseline=dict(package=version, build=int(version.split('.')[-1]), source=source,
                              setup_sha256=digest(setup), package_sha256=digest(package), signed=False))
