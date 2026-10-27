"""Beta-only AppImage publication under the existing Linux signing authority/lock."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import tempfile

import appimage_artifact as artifact
import release_notes
spec = importlib.util.spec_from_file_location('arch_signing', Path(__file__).parents[1] / 'arch/publish.py')
arch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(arch)
PREFIX = 'okilum/linux'
FINGERPRINT = '7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882'


def encode(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def validate(directory, source, receipt, *, allow_acceptance_harness=False):
    original = (directory / 'provenance.json').read_bytes()
    provenance = json.loads(original)
    run = receipt['run']
    build = 5000 + run['run_number']
    image = (directory / artifact.IMAGE).read_bytes()
    if (not re.fullmatch('[0-9a-f]{40}', source)
            or provenance.get('source_sha') != source or provenance.get('artifact') != artifact.IMAGE
            or provenance.get('signed') is not False
            or (provenance.get('acceptance_harness', False) is not False and not allow_acceptance_harness)
            or provenance.get('sha256') != digest(image)
            or provenance.get('workflow_run') != str(run['id'])
            or provenance.get('workflow_attempt') != str(run['run_attempt'])
            or provenance.get('build_version') != str(build)
            or provenance.get('release_version') != f'0.1.{build}'
            or build <= 5000):
        raise ValueError('AppImage original source/version/checksum/provenance mismatch')
    return build, image, original


def signature(store, key, tmp, prefix, name, data):
    path = tmp / name
    path.write_bytes(data)
    existing = store.call('GET', f'{prefix}/{name}.sig')
    if existing is None:
        key.sign(path)
        existing = Path(str(path) + '.sig').read_bytes()
    else:
        Path(str(path) + '.sig').write_bytes(existing)
    key.verify(path)
    return existing


def publish(store, key, tmp, directory, source, receipt, publication_build, archive_only=False, *, client):
    build, image, original = validate(directory, source, receipt)
    if key.fingerprint != FINGERPRINT:
        raise ValueError('AppImage requires the existing pinned Linux signing authority')
    if not isinstance(publication_build, int) or publication_build <= 5000:
        raise ValueError('Invalid separate Forgejo publication number')
    archive = f'{PREFIX}/builds/{build}'
    current_raw = store.call('GET', f'{PREFIX}/beta/latest.json')
    current = json.loads(current_raw) if current_raw is not None else None
    run, hosted_artifact = receipt['run'], receipt['artifact']
    # Publication numbering is operational metadata, never the embedded executable version.
    descriptor = {'schema': 1, 'platform': 'linux-appimage', 'channel': 'beta',
                  'build': build, 'version': f'0.1.{build}', 'source': source,
                  'filename': artifact.IMAGE, 'sha256': digest(image), 'size': len(image),
                  'key': f'{archive}/{artifact.IMAGE}', 'fingerprint': FINGERPRINT,
                  'provenance_sha256': digest(original),
                  'github': {'repository': artifact.REPOSITORY, 'workflow': artifact.WORKFLOW,
                             'run': run['id'], 'attempt': run['run_attempt'],
                             'artifact': hosted_artifact['id'], 'digest': hosted_artifact['digest']},
                  'publication_build': publication_build}
    previous = store.call('GET', f'{archive}/release.json')
    if previous is not None:
        old = json.loads(previous)
        # Retry under another Forgejo run retains the first publication number/signatures.
        if {k: v for k, v in old.items() if k != 'publication_build'} != {k: v for k, v in descriptor.items() if k != 'publication_build'}:
            raise ValueError('AppImage build already has conflicting contents or identity')
        descriptor = old
    encoded = encode(descriptor)
    if current is not None:
        if current['build'] == build and current != descriptor:
            raise ValueError('Conflicting beta identity for the same AppImage build')
        if current['build'] > build:
            archive_only = True
    notes = None if archive_only else release_notes.prepare_appimage(store, client, build, source)
    # Preflight every immutable conflict and verify reused signatures before ANY write.
    values = {artifact.IMAGE: image, 'provenance.json': original, 'release.json': encoded}
    for name, data in values.items():
        existing = store.call('GET', f'{archive}/{name}')
        if existing is not None and existing != data:
            raise ValueError('Conflicting immutable AppImage asset: ' + name)
    for name in [artifact.IMAGE, 'release.json']:
        values[name + '.sig'] = signature(store, key, tmp, archive, name, values[name])
    for name, data in values.items():
        if store.call('GET', f'{archive}/{name}') is None:
            store.put(f'{archive}/{name}', data, 'application/json' if name.endswith('.json') else 'application/octet-stream',
                      'public, max-age=31536000, immutable')
    # Read back all immutable bytes before exposing any moving beta asset.
    for name, data in values.items():
        if store.call('GET', f'{archive}/{name}') != data:
            raise ValueError('Immutable AppImage readback failed: ' + name)
    if not archive_only:
        release_notes.record(store, notes)
        for name in [artifact.IMAGE, artifact.IMAGE + '.sig']:
            store.put(f'{PREFIX}/beta/{name}', values[name], 'application/octet-stream', 'no-cache')
        # Clients verify this signature and referenced immutable bytes. Crossing a detached
        # pair update fails closed; latest.json is the final public commit point.
        store.put(f'{PREFIX}/beta/latest.json.sig', values['release.json.sig'], 'application/octet-stream', 'no-cache')
        store.put(f'{PREFIX}/beta/latest.json', encoded, 'application/json', 'no-cache')
    print(f'AppImage {build} source {source}: ' + ('archive only' if archive_only else 'beta'))
    return descriptor


def notes_client():
    # Same authenticated Forgejo client as the other release publishers.
    import publication
    return publication.Forgejo()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--publication-build', type=int, required=True)
    parser.add_argument('--archive-only', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as temporary:
        tmp = Path(temporary)
        receipt = artifact.unpack(args.directory, tmp / 'payload', args.source)
        # No credential import before original bytes/source/version are checked.
        validate(tmp / 'payload', args.source, receipt)
        with arch.SigningKey() as key:
            publish(arch.R2(), key, tmp, tmp / 'payload', args.source, receipt,
                    args.publication_build, args.archive_only, client=notes_client())
