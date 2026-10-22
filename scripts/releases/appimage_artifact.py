"""Resolve accepted exact-source hosted bytes; bridge-only GitHub credential use."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import urllib.request
import io
import zipfile

spec = importlib.util.spec_from_file_location('appimage_transport', Path(__file__).parents[1] / 'ci/github-macos.py')
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
REPOSITORY = 'BeFeast/okilum'
WORKFLOW = '.github/workflows/forgejo-linux.yml'
IMAGE = 'Okilum-x86_64.AppImage'
SMOKE = 'Verify unchanged AppImage on clean Ubuntu and Fedora'


def accepted(run, jobs, source):
    if (run.get('repository', {}).get('full_name') != REPOSITORY
            or run.get('head_sha') != source or run.get('path') != WORKFLOW
            or run.get('event') != 'push' or run.get('status') != 'completed'
            or run.get('conclusion') != 'success' or run.get('run_attempt') != 1
            or not isinstance(run.get('run_number'), int) or run['run_number'] <= 0
            or not (run.get('head_branch') == 'main'
                    or run.get('head_branch', '').startswith('forgejo-linux-pr/'))):
        raise ValueError('Not an accepted exact-source hosted Linux run')
    for name, step in [('appimage', SMOKE), ('linux', 'Run full Linux gate')]:
        matches = [j for j in jobs if j.get('name') == name]
        if (len(matches) != 1 or matches[0].get('run_id') != run['id']
                or matches[0].get('head_sha') != source
                or matches[0].get('status') != 'completed'
                or matches[0].get('conclusion') != 'success'
                or len([s for s in matches[0].get('steps', [])
                        if s.get('name') == step and s.get('conclusion') == 'success']) != 1):
            raise ValueError('Hosted smoke or Linux aggregate did not execute successfully')


def artifact_identity(artifact, run, source):
    if (artifact.get('name') != f'linux-appimage-{source}' or artifact.get('expired') is not False
            or artifact.get('workflow_run', {}).get('id') != run['id']
            or artifact.get('workflow_run', {}).get('head_sha') != source
            or not re.fullmatch('sha256:[0-9a-f]{64}', artifact.get('digest', ''))
            or not isinstance(artifact.get('id'), int) or artifact['id'] <= 0):
        raise ValueError('Missing or mismatched hosted artifact identity/digest')


class DownloadRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if not newurl.startswith('https://'):
            raise ValueError('Artifact redirect requires HTTPS')
        redirected.remove_header('Authorization')
        return redirected


def download(token, artifact_id):
    request = urllib.request.Request(
        f'https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{artifact_id}/zip',
        headers={'Authorization': f'Bearer {token}', 'Accept': 'application/vnd.github+json'})
    with urllib.request.build_opener(DownloadRedirect()).open(request, timeout=300) as response:
        return response.read()


def stage(api, source, directory, fetch):
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Expected full main source SHA')
    # No dispatch/retry/rebuild here. Missing, failed, expired or ambiguous evidence stops publication.
    runs = api.request(f'actions/workflows/forgejo-linux.yml/runs?head_sha={source}&event=push&per_page=100')
    matches = [r for r in runs['workflow_runs'] if r.get('head_sha') == source and r.get('path') == WORKFLOW]
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous exact-source hosted producer; nothing published')
    run = api.request(f"actions/runs/{matches[0]['id']}")
    jobs = api.request(f"actions/runs/{run['id']}/attempts/1/jobs?per_page=100")
    if jobs.get('total_count', len(jobs['jobs'])) != len(jobs['jobs']):
        raise ValueError('Incomplete hosted job evidence')
    accepted(run, jobs['jobs'], source)
    artifacts = api.request(f"actions/runs/{run['id']}/artifacts?per_page=100")
    if artifacts.get('total_count', len(artifacts['artifacts'])) != len(artifacts['artifacts']):
        raise ValueError('Incomplete artifact listing')
    matches = [a for a in artifacts['artifacts'] if a['name'] == f'linux-appimage-{source}']
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous AppImage artifact')
    artifact = matches[0]
    artifact_identity(artifact, run, source)
    data = fetch(artifact['id'])
    if 'sha256:' + hashlib.sha256(data).hexdigest() != artifact['digest']:
        raise ValueError('GitHub artifact archive digest mismatch')
    # Re-read after transfer: a rerun cannot silently change acceptance under these bytes.
    if api.request(f"actions/runs/{run['id']}") != run:
        raise ValueError('Hosted run changed during transfer')
    directory.mkdir(parents=True, exist_ok=False)
    (directory / 'artifact.zip').write_bytes(data)
    (directory / 'acceptance.json').write_text(json.dumps({'run': run, 'jobs': jobs['jobs'], 'artifact': artifact}, sort_keys=True))
    print(f"Accepted AppImage run {run['id']} attempt 1 artifact {artifact['id']}: {artifact['digest']}")


def unpack(directory, destination, source):
    import publication
    receipt = json.loads((directory / 'acceptance.json').read_bytes())
    run, artifact = receipt['run'], receipt['artifact']
    accepted(run, receipt['jobs'], source)
    artifact_identity(artifact, run, source)
    data = (directory / 'artifact.zip').read_bytes()
    if 'sha256:' + hashlib.sha256(data).hexdigest() != artifact['digest']:
        raise ValueError('Transferred archive differs from accepted GitHub digest')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive.namelist()
        if len(names) != 4 or set(names) != {IMAGE, 'provenance.json', 'SHA256SUMS', 'bundled-libraries.txt'}:
            raise ValueError('Unexpected, missing or duplicate AppImage archive member')
    publication.extract(data, destination)
    return receipt


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True)
    parser.add_argument('--directory', type=Path, required=True)
    args = parser.parse_args()
    token = os.environ['MIRROR_TOKEN']
    stage(transport.GitHub(token), args.source, args.directory, lambda aid: download(token, aid))
