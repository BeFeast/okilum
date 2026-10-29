"""Admit an original hosted portable bundle for the fixed canonical driver."""
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import zipfile
import xml.etree.ElementTree as ET


def sha(data):
    return hashlib.sha256(data).hexdigest()


def members(archive):
    result = {}
    with zipfile.ZipFile(io.BytesIO(archive)) as bundle:
        for item in bundle.infolist():
            name = item.filename
            path = PurePosixPath(name)
            if (path.is_absolute() or '..' in path.parts or '\\' in name
                    or ':' in name or str(path) != name.rstrip('/')
                    or (item.external_attr >> 16) & 0o170000 == 0o120000):
                raise ValueError('Unsafe bundle member')
            for part in path.parts:
                if (part.endswith(('.', ' ')) or any(ord(c) < 32 for c in part)
                        or re.fullmatch(r'(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?', part, re.I)):
                    raise ValueError('Windows reserved member')
            if name in result or name.casefold() in {n.casefold() for n in result}:
                raise ValueError('Duplicate bundle member')
            if not item.is_dir():
                result[name] = bundle.read(item)
    names = {n.casefold() for n in result}
    for name in names:
        if any(str(parent) in names for parent in PurePosixPath(name).parents if str(parent) != '.'):
            raise ValueError('File/directory collision')
    return result


def admit(manifest_path):
    manifest = json.loads(Path(manifest_path).read_text())
    source = manifest['source']
    if not re.fullmatch('[a-f0-9]{40}', source):
        raise ValueError('Exact source required')
    run = json.loads(Path(manifest['hosted_run']).read_text())
    metadata = json.loads(Path(manifest['artifact_metadata']).read_text())
    if (run['head_sha'] != source or run['status'] != 'completed'
            or run['conclusion'] != 'success'
            or run['path'] != '.github/workflows/forgejo-windows-release.yml'
            or metadata['workflow_run']['head_sha'] != source
            or metadata['workflow_run']['id'] != run['id']
            or metadata['expired'] or metadata['name'] != 'unsigned-windows'):
        raise ValueError('Exact-source packaging provenance mismatch')
    original = Path(manifest['artifact_zip']).read_bytes()
    if metadata['digest'] != 'sha256:' + sha(original):
        raise ValueError('Original hosted artifact digest mismatch')
    outer = members(original)
    name = 'BeFeast.Okilum-beta-Portable.zip'
    if name not in outer:
        raise ValueError('Original producer portable ZIP required; no repackaging fallback')
    payload = members(outer[name])
    # The real Velopack producer includes its launcher/updater but marks this
    # distribution portable. Preserve those original bytes; never launch them.
    required = {'.portable', 'Okilum.exe', 'Update.exe', 'current/okilum.exe', 'current/sq.version'}
    if not required.issubset(payload) or payload['current/okilum.exe'][:2] != b'MZ':
        raise ValueError('Original portable layout required')
    if not any(n.startswith('current/gpui-shaders/') for n in payload):
        raise ValueError('Shader bundle missing')
    root = ET.fromstring(payload['current/sq.version'])
    versions = [n.text for n in root.iter() if n.tag.split('}')[-1] == 'version']
    if len(versions) != 1 or not re.fullmatch(r'0\.1\.[0-9]+', versions[0]):
        raise ValueError('Portable package version ambiguous')
    return dict(source=source, version=versions[0], original_artifact_sha256=sha(original),
                original_bundle_name=name, bundle_sha256=sha(outer[name]),
                executable='current/okilum.exe',
                files={n: sha(data) for n, data in payload.items()},
                bundle=outer[name])



def stage(manifest, output):
    output = Path(output)
    result = admit(manifest)
    output.mkdir(exist_ok=False)
    (output / 'app.zip').write_bytes(result.pop('bundle'))
    (output / 'admission.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    import sys
    stage(sys.argv[1], sys.argv[2])
