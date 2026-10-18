#!/usr/bin/env python3
"""Reuse the exact signed producer-build package before makepkg can change metadata."""
import argparse
import importlib.util
import json
from pathlib import Path
import tempfile

spec = importlib.util.spec_from_file_location('arch_consumer', Path(__file__).resolve().parents[1] / 'ci/github-linux.py')
consumer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(consumer)
payload = consumer.payload


def reuse(source, prebuilt, output, *, fetch=consumer.public_read, verify=consumer.verify_signature):
    expected = json.loads((prebuilt / 'receipt.json').read_text())
    # Unsigned structural fields may locate an archive, but cannot authorize
    # bytes. Authenticate against its signature before consulting receipt authority
    # that may no longer exist after hosted expiry/interrupted publication.
    m = payload.validate_manifest(expected.get('manifest', {}), source, payload.config(),
                                  expected.get('producer'))
    build = m['build']
    name = f'okilum-0.1.{build}-1-x86_64.pkg.tar.zst'
    prefix = f'okilum/arch/builds/{build}'
    # The build archive is committed before feed/catalog publication. A source
    # descriptor alone misses recovery after that archive commit succeeded.
    raw = fetch(f'{prefix}/manifest.json')
    if raw is None:
        payload.require(fetch(f'{prefix}/{name}') is None and fetch(f'{prefix}/{name}.sig') is None,
                        'Incomplete Arch archive; explicit recovery required before packaging')
        # No signed envelope exists. Preserve independent authority for unsigned
        # inputs before allowing the caller to continue to initial packaging.
        payload.validate_receipt(expected, source, m['config'])
        return False
    archived = json.loads(raw)
    payload.require(archived.get('build') == build and archived.get('source') == source
                    and archived.get('filename') == name, 'Wrong archived package identity')
    package = fetch(f'{prefix}/{name}')
    signature = fetch(f'{prefix}/{name}.sig')
    payload.require(package is not None and signature is not None
                    and payload.digest(package) == archived.get('sha256'), 'Archived package missing/checksum mismatch')
    with tempfile.TemporaryDirectory() as temporary:
        path = Path(temporary) / name
        path.write_bytes(package)
        Path(str(path) + '.sig').write_bytes(signature)
        verify(path, Path(str(path) + '.sig'))
        original, binary = payload.package_receipt(path, source, build, trusted_envelope=True)
        payload.validate_receipt(original, source, m['config'], trusted_envelope=True)
    # Verification/QA timestamps can advance on a fresh hosted download. All
    # immutable artifact and producer identities must still be exactly equal.
    fields = ('manifest', 'producer', 'artifact_id', 'artifact_api_digest',
              'artifact_zip_sha256', 'archive_sha256')
    selected_binary = prebuilt / 'payload/okilum'
    payload.require(all(original[k] == expected.get(k) for k in fields)
                    and archived.get('production') == original
                    and archived.get('binary_sha256') == m['binary_sha256']
                    and selected_binary.is_file() and not selected_binary.is_symlink()
                    and binary == selected_binary.read_bytes(),
                    'Archived package differs from selected production payload')
    # The selected immutable fields/bytes now match the authenticated envelope.
    # Only mutable receipt timing fields remain to be structurally validated.
    payload.validate_receipt(expected, source, m['config'], trusted_envelope=True)
    output.mkdir(parents=True, exist_ok=True)
    existing = list(output.glob('okilum-*.pkg.tar.zst'))
    payload.require(not existing or (len(existing) == 1 and existing[0].name == name
                    and not existing[0].is_symlink() and existing[0].read_bytes() == package),
                    'Existing package differs from immutable archive')
    (output / name).write_bytes(package)
    (output / (name + '.sig')).write_bytes(signature)
    # Publication compares the standalone receipt with the one embedded in the
    # package. Retain the original receipt along with the original package bytes.
    (output / 'production-receipt.json').write_bytes(payload.encode(original))
    return True


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--prebuilt', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    print('reused' if reuse(args.source, args.prebuilt, args.output) else 'missing')
