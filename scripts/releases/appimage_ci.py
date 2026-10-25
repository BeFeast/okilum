"""Hosted disposable-key composition test; acceptance statuses are fixtures, not authority."""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

import appimage_publish as publisher


class LocalStore:
    def __init__(self, root):
        self.root = root
        self.writes = []
        self.fail = None
        self.corrupt = None

    def call(self, method, key):
        assert method == 'GET'
        path = self.root / key
        data = path.read_bytes() if path.exists() else None
        return b'corrupt readback' if key == self.corrupt and data is not None else data

    def put(self, key, data, *_):
        if key == self.fail:
            raise RuntimeError('injected interrupted upload')
        path = self.root / key
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        self.writes.append(key)


def rejected(action, exceptions=(ValueError, subprocess.CalledProcessError)):
    try:
        action()
    except exceptions:
        return
    raise AssertionError('Negative control was accepted')


def handoff(root, directory):
    artifact = publisher.artifact
    source = os.environ['GITHUB_SHA']
    api = artifact.transport.GitHub(os.environ['GH_TOKEN'])
    run = api.request('actions/runs/' + os.environ['GITHUB_RUN_ID'])
    assert run['id'] == int(os.environ['GITHUB_RUN_ID'])
    assert run['run_number'] == int(os.environ['GITHUB_RUN_NUMBER'])
    # Platform step outcomes cannot distinguish transport from signing failures.
    # Producer log text is never authority for relaxing this boundary.
    if run.get('run_attempt') != 1 or os.environ['GITHUB_RUN_ATTEMPT'] != '1':
        raise ValueError('CI recovery beyond original attempt is unsupported')
    item = api.request('actions/artifacts/' + os.environ['APPIMAGE_ARTIFACT_ID'])
    artifact.artifact_identity(item, run, source)
    data = artifact.download(os.environ['GH_TOKEN'], item['id'])
    if item['digest'] != 'sha256:' + publisher.digest(data):
        raise ValueError('Hosted archive digest mismatch')
    # The workflow cannot be completed while this test is running. Only completion
    # statuses/job gates are fixtures. Source/run/attempt/artifact/digest are real.
    # This receipt stays in temporary test storage and MUST NOT authorize publication.
    run = {**run, 'status': 'completed', 'conclusion': 'success'}
    jobs = [{'name': name, 'run_id': run['id'], 'head_sha': source,
             'status': 'completed', 'conclusion': 'success',
             'steps': [{'name': step, 'conclusion': 'success'}]}
            for name, step in [('appimage', artifact.SMOKE), ('linux', 'Run full Linux gate')]]
    receipt = {'run': run, 'jobs': jobs, 'artifact': item}
    stage = root / 'handoff'; stage.mkdir()
    (stage / 'acceptance.json').write_text(json.dumps(receipt))
    (stage / 'artifact.zip').write_bytes(data)
    payload = root / 'payload'
    receipt = artifact.unpack(stage, payload, source)
    publisher.validate(payload, source, receipt)  # Before importing any signing key.
    # Reuse only bytes from the actual digest-verified hosted archive. Locally
    # self-consistent producer files cannot substitute for that archive identity.
    for name in [artifact.IMAGE, 'provenance.json', 'SHA256SUMS', 'bundled-libraries.txt']:
        if (payload / name).read_bytes() != (directory / name).read_bytes():
            raise ValueError('Hosted archive differs from same-job producer files')
    for field, value in [('head_sha', '0' * 40), ('run_attempt', 2)]:
        broken = copy.deepcopy(receipt); broken['run'][field] = value
        (stage / 'acceptance.json').write_text(json.dumps(broken))
        rejected(lambda: artifact.unpack(stage, root / 'bad', source))
    broken = copy.deepcopy(receipt); broken['artifact']['digest'] = 'sha256:' + '0' * 64
    (stage / 'acceptance.json').write_text(json.dumps(broken))
    rejected(lambda: artifact.unpack(stage, root / 'bad', source))
    (stage / 'acceptance.json').write_text(json.dumps(receipt))
    original = (payload / 'provenance.json').read_bytes()
    provenance = json.loads(original)
    for field, value in [('source_sha', '0' * 40), ('workflow_attempt', '2'), ('sha256', '0' * 64)]:
        (payload / 'provenance.json').write_text(json.dumps({**provenance, field: value}))
        rejected(lambda: publisher.validate(payload, source, receipt))
    (payload / 'provenance.json').write_bytes(original)
    rejected(lambda: artifact.unpack(stage, root / 'bad', '0' * 40))
    (stage / 'artifact.zip').write_bytes(data + b'tampered')
    rejected(lambda: artifact.unpack(stage, root / 'bad', source))
    (stage / 'artifact.zip').write_bytes(data)
    print(f"APPIMAGE_HANDOFF_PASS: source={source} run={run['id']} attempt=1 "
          f"artifact={item['id']} digest={item['digest']}; completion statuses are test fixtures")
    return payload, source, receipt


def main(directory):
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        payload, source, receipt = handoff(root, directory)
        home = root / 'key'; home.mkdir(mode=0o700)
        verifier = root / 'verifier'; verifier.mkdir(mode=0o700)
        subprocess.run(['gpg', '--homedir', str(home), '--batch', '--passphrase', '',
                        '--quick-generate-key', 'AppImage CI <ci@example.invalid>', 'ed25519', 'sign', '1d'], check=True)
        try:
            info = publisher.arch.run('gpg', '--homedir', str(home), '--with-colons', '--list-secret-keys').decode()
            fingerprint = next(line.split(':')[9] for line in info.splitlines() if line.startswith('fpr:'))
            secret = publisher.arch.run('gpg', '--homedir', str(home), '--armor', '--export-secret-keys').decode()
            work = root / 'work'; work.mkdir()
            # Test-only substitution; the production CLI still pins the existing authority.
            with patch.dict(os.environ, ARCH_GPG_PRIVATE_KEY=secret, ARCH_GPG_FINGERPRINT=fingerprint), \
                    patch.object(publisher, 'FINGERPRINT', fingerprint), publisher.arch.SigningKey() as key:
                def publish(store, number=9001):
                    return publisher.publish(store, key, work, payload, source, receipt, number)

                build = publisher.validate(payload, source, receipt)[0]
                archive = f'okilum/linux/builds/{build}'
                beta = 'okilum/linux/beta'
                image_name = publisher.artifact.IMAGE
                # Real GPG signatures, with corrupt readback before any moving write.
                damaged = LocalStore(root / 'damaged')
                damaged.corrupt = archive + '/provenance.json'
                rejected(lambda: publish(damaged))
                assert not any('/beta/' in k for k in damaged.writes)
                # Interrupted immutable upload recovers, reusing its actual signature.
                store = LocalStore(root / 'store')
                store.fail = archive + '/release.json.sig'
                rejected(lambda: publish(store), (RuntimeError,))
                assert not any('/beta/' in k for k in store.writes)
                original_signature = store.call('GET', archive + '/' + image_name + '.sig')
                assert original_signature
                store.fail = None
                descriptor = publish(store)
                assert original_signature == store.call('GET', archive + '/' + image_name + '.sig')
                before = {k: store.call('GET', k) for k in store.writes}
                with patch.object(key, 'sign', side_effect=AssertionError('retry must reuse signatures')):
                    assert publish(store, 9002) == descriptor
                assert before == {k: store.call('GET', k) for k in store.writes}
                # A client crossing the moving descriptor/signature pair fails closed.
                old = b'{"build":5000}\n'
                store.put(beta + '/latest.json', old)
                store.fail = beta + '/latest.json.sig'
                rejected(lambda: publish(store), (RuntimeError,))
                assert store.call('GET', beta + '/latest.json') == old
                store.fail = beta + '/latest.json'
                rejected(lambda: publish(store), (RuntimeError,))
                assert store.call('GET', beta + '/latest.json') == old

                def verify(path, expected=fingerprint):
                    status = publisher.arch.run('gpg', '--homedir', str(verifier), '--batch',
                        '--status-fd', '1', '--verify', str(path) + '.sig', str(path)).decode()
                    signatures = [line.split() for line in status.splitlines() if line.startswith('[GNUPG:] VALIDSIG ')]
                    if len(signatures) != 1 or expected not in (signatures[0][2], signatures[0][-1]):
                        raise ValueError('Unexpected public signer')

                installed = root / 'install'; installed.mkdir()
                latest = installed / 'latest.json'
                def download_latest():
                    latest.write_bytes(store.call('GET', beta + '/latest.json'))
                    Path(str(latest) + '.sig').write_bytes(store.call('GET', beta + '/latest.json.sig'))
                download_latest()
                rejected(lambda: verify(latest))  # Positive signature, absent public key.
                publisher.arch.run('gpg', '--homedir', str(verifier), '--batch', '--import', input=key.public())
                assert 'sec:' not in publisher.arch.run('gpg', '--homedir', str(verifier), '--with-colons', '--list-secret-keys').decode()
                rejected(lambda: verify(latest))  # Now imported, but interrupted pair.
                store.fail = None
                assert publish(store, 9003) == descriptor
                download_latest()
                verify(latest)
                rejected(lambda: verify(latest, '0' * 40))
                downloaded = json.loads(latest.read_bytes())
                assert downloaded == descriptor
                image = installed / downloaded['filename']
                image.write_bytes(store.call('GET', downloaded['key']))
                Path(str(image) + '.sig').write_bytes(store.call('GET', downloaded['key'] + '.sig'))
                verify(image)
                assert publisher.digest(image.read_bytes()) == downloaded['sha256']
                assert image.stat().st_size == downloaded['size']
                assert publisher.digest(store.call('GET', archive + '/provenance.json')) == downloaded['provenance_sha256']
                image.chmod(0o755)
                subprocess.run(['bash', 'scripts/appimage/launch.sh', str(image), '--help'], check=True)
                for path in [image, latest]:
                    original = path.read_bytes()
                    path.write_bytes(original + b'tampered')
                    rejected(lambda: verify(path))
                    path.write_bytes(original)
                # Conflicting immutable bytes and a tampered reused signature: zero writes.
                for name in [image_name, image_name + '.sig']:
                    path = store.root / archive / name
                    original = path.read_bytes(); path.write_bytes(original + b'conflict')
                    count = len(store.writes)
                    rejected(lambda: publish(store))
                    assert len(store.writes) == count
                    path.write_bytes(original)
            print('APPIMAGE_SIGNED_INSTALL_PASS: actual archive unpack/validate/sign; public-only verifier; '
                  'immutable payload/descriptor; missing/wrong signer; tamper; readback; interruptions; retry; conflict; help')
        finally:
            for gpg_home in [home, verifier]:
                subprocess.run(['gpgconf', '--homedir', str(gpg_home), '--kill', 'all'], check=True)


if __name__ == '__main__':
    # Explicit disposable-key harness only. Production CLI rejects QA artifacts.
    production_validate = publisher.validate
    with patch.object(publisher, 'validate', lambda *args: production_validate(*args, allow_acceptance_harness=True)):
        main(Path(sys.argv[1]).resolve())
