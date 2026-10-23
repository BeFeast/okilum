"""Hosted disposable-key composition test; acceptance statuses are fixtures, not authority."""
import json
import os
import re
import shutil
import urllib.request
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


SIGNATURE = 'Verify signed AppImage installation with disposable CI key'


def job_log(token, job_id):
    artifact = publisher.artifact
    request = urllib.request.Request(
        f'https://api.github.com/repos/{artifact.REPOSITORY}/actions/jobs/{job_id}/logs',
        headers={'Authorization': f'Bearer {token}'})
    with urllib.request.build_opener(artifact.DownloadRedirect()).open(request, timeout=30) as response:
        data = response.read(2 * 1024 * 1024 + 1)
    if len(data) > 2 * 1024 * 1024:
        raise ValueError('Prior job log exceeds recovery evidence bound')
    return data.decode('utf-8')


def verify_attempt(api, run, fetch_log):
    """CI-only recovery. Production resolvers still require original attempt 1."""
    artifact = publisher.artifact
    if (run.get('repository', {}).get('full_name') != artifact.REPOSITORY
            or run.get('path') != artifact.WORKFLOW or run.get('event') != 'push'
            or not re.fullmatch('[0-9a-f]{40}', run.get('head_sha', ''))
            or not (run.get('head_branch') == 'main'
                    or run.get('head_branch', '').startswith('forgejo-linux-pr/'))
            or any(type(run.get(field)) is not int or run[field] <= 0
                   for field in ('id', 'run_number', 'workflow_id'))
            or type(run.get('run_attempt')) is not int or not 1 <= run['run_attempt'] <= 3):
        raise ValueError('Untrusted CI recovery origin/attempt')
    fields = ('id', 'run_number', 'workflow_id', 'path', 'head_sha', 'head_branch', 'event', 'repository')
    for attempt in range(1, run['run_attempt']):
        prior = api.request(f"actions/runs/{run['id']}/attempts/{attempt}")
        if (any(prior.get(field) != run.get(field) for field in fields)
                or prior.get('run_attempt') != attempt
                or prior.get('status') != 'completed' or prior.get('conclusion') != 'failure'):
            raise ValueError('Prior attempt identity/outcome mismatch')
        response = api.request(f"actions/runs/{run['id']}/attempts/{attempt}/jobs?per_page=100")
        jobs = response['jobs']
        if response.get('total_count', len(jobs)) != len(jobs) or len(jobs) != 3:
            raise ValueError('Incomplete prior attempt jobs')
        for name, outcome in [('linux-checks', 'success'), ('appimage', 'failure'), ('linux', 'failure')]:
            matches = [job for job in jobs if job.get('name') == name]
            if (len(matches) != 1 or matches[0].get('run_id') != run['id']
                    or matches[0].get('run_attempt') != attempt
                    or matches[0].get('head_sha') != run['head_sha']
                    or matches[0].get('status') != 'completed'
                    or matches[0].get('conclusion') != outcome):
                raise ValueError('Prior attempt gate/identity mismatch')
        checks = next(job for job in jobs if job['name'] == 'linux-checks')
        if len([step for step in checks.get('steps', [])
                if step.get('name') == 'Run full Linux gate'
                and step.get('status') == 'completed' and step.get('conclusion') == 'success']) != 1:
            raise ValueError('Prior Linux checks did not execute')
        job = next(job for job in jobs if job['name'] == 'appimage')
        steps = job['steps']
        for name in ['Build exact-source AppImage once', artifact.SMOKE]:
            if len([step for step in steps if step.get('name') == name
                    and step.get('status') == 'completed' and step.get('conclusion') == 'success']) != 1:
                raise ValueError('Prior build/smoke was not successful')
        failed = [step for step in steps if step.get('conclusion') != 'success']
        if (len(failed) != 1 or failed[0].get('name') != SIGNATURE
                or failed[0].get('status') != 'completed' or failed[0].get('conclusion') != 'failure'):
            raise ValueError('Prior failure was not confined to signature transport')
        log = fetch_log(job['id'])
        # Require the actual pre-key-import download traceback, not arbitrary text
        # mentioning HTTP 503. Legacy egress failures remain recoverable.
        if (log.count('Traceback (most recent call last):') != 1
                or 'in handoff' not in log
                or "data = artifact.download(os.environ['GH_TOKEN'], item['id'])" not in log
                or not re.search(r'urllib.error.HTTPError: HTTP Error 503: Egress is over the account limit\.?\s', log)
                or 'APPIMAGE_SIGNED_INSTALL_PASS' in log or 'gpg: key' in log):
            raise ValueError('Prior failure lacks verified pre-signing egress traceback')


def current_run():
    artifact = publisher.artifact
    source = os.environ['GITHUB_SHA']
    token = os.environ['GH_TOKEN']
    api = artifact.transport.GitHub(token)
    run = api.request('actions/runs/' + os.environ['GITHUB_RUN_ID'])
    assert run['id'] == int(os.environ['GITHUB_RUN_ID'])
    assert run['run_number'] == int(os.environ['GITHUB_RUN_NUMBER'])
    assert run['run_attempt'] == int(os.environ['GITHUB_RUN_ATTEMPT'])
    assert run['head_sha'] == source
    verify_attempt(api, run, lambda job: job_log(token, job))
    return api, run, source


def handoff(root, directory):
    artifact = publisher.artifact
    api, run, source = current_run()
    item = api.request('actions/artifacts/' + os.environ['APPIMAGE_ARTIFACT_ID'])
    artifact.artifact_identity(item, run, source)
    # Reuse the exact producer bytes already smoke-tested in this job. Hosted ZIP
    # transfer/digest acceptance remains mandatory in production stage/unpack;
    # this disposable-key harness does not issue a publication receipt.
    payload = root / 'payload'; payload.mkdir()
    for name in [artifact.IMAGE, 'provenance.json', 'SHA256SUMS', 'bundled-libraries.txt']:
        shutil.copyfile(directory / name, payload / name)
    receipt = {'run': run, 'artifact': item, 'jobs': []}
    publisher.validate(payload, source, receipt)
    expected = publisher.digest((payload / artifact.IMAGE).read_bytes()) + '  ' + artifact.IMAGE
    if (payload / 'SHA256SUMS').read_text().strip() != expected:
        raise ValueError('Local producer checksum mismatch')
    if api.request(f"actions/runs/{run['id']}") != run:
        raise ValueError('CI run changed during local reuse')
    original = (payload / 'provenance.json').read_bytes()
    provenance = json.loads(original)
    for field, value in [('source_sha', '0' * 40),
                         ('workflow_attempt', str(run['run_attempt'] + 1)), ('sha256', '0' * 64)]:
        (payload / 'provenance.json').write_text(json.dumps({**provenance, field: value}))
        rejected(lambda: publisher.validate(payload, source, receipt))
    (payload / 'provenance.json').write_bytes(original)
    rejected(lambda: publisher.validate(payload, '0' * 40, receipt))
    print(f"APPIMAGE_LOCAL_REUSE_PASS: source={source} run={run['id']} attempt={run['run_attempt']} "
          f"artifact={item['id']} producer_sha256={provenance['sha256']}; "
          'disposable-key local composition only; hosted ZIP acceptance is separate')
    return payload, source, receipt


def main(directory):
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        payload, source, receipt = handoff(root, directory)
        # The signing harness must preserve the smoke-tested producer bytes.
        for name in [publisher.artifact.IMAGE, 'provenance.json', 'SHA256SUMS', 'bundled-libraries.txt']:
            assert (payload / name).read_bytes() == (directory / name).read_bytes()
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
            print('APPIMAGE_SIGNED_INSTALL_PASS: local producer validate/sign; public-only verifier; '
                  'immutable payload/descriptor; missing/wrong signer; tamper; readback; interruptions; retry; conflict; help')
        finally:
            for gpg_home in [home, verifier]:
                subprocess.run(['gpgconf', '--homedir', str(gpg_home), '--kill', 'all'], check=True)


if __name__ == '__main__':
    if sys.argv[1:] == ['--verify-attempt']:
        _, run, _ = current_run()
        print(f"APPIMAGE_CI_ATTEMPT_VERIFIED run={run['id']} attempt={run['run_attempt']}")
    else:
        main(Path(sys.argv[1]).resolve())
