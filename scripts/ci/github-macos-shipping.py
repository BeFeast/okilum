#!/usr/bin/env python3
"""Trusted, manual Forgejo -> hosted Mac shipping-artifact bridge. No feed writes."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import base64
from datetime import datetime, timezone
import time

spec = importlib.util.spec_from_file_location('macos_bridge', Path(__file__).with_name('github-macos.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
WORKFLOW = '.github/workflows/forgejo-macos-shipping.yml'
STEP = 'Build, sign and notarize exact product source'
MIRROR_WAIT_SECONDS = 600


def read(api, path):
    """GET with the bridge's bounded backoff (#2589: one GitHub 502 while the
    hosted build ran failed the hourly beta). Never used for the dispatch POST."""
    return bridge.hosted_request(api, path, sleep=time.sleep)


def beta_publication(env, source):
    return (env.get('VERIFY_ONLY') != 'true' and env.get('GITHUB_REF') == 'refs/heads/main'
            and (source == env.get('GITHUB_SHA') or source == env.get('HOURLY_SOURCE'))
            and env.get('GITHUB_EVENT_NAME') in ('push', 'schedule', 'workflow_dispatch'))


def identity(env):
    if (env.get('GITHUB_REF') != 'refs/heads/main'
            or env.get('GITHUB_EVENT_NAME') not in ('workflow_dispatch', 'push', 'schedule')):
        raise ValueError('Hosted shipping artifacts require a trusted main push or manual run')
    if env.get('GITHUB_RUN_ATTEMPT') != '1':
        raise ValueError('Shipping producer reruns are forbidden')
    source = env.get('PRODUCT_SOURCE', '')
    tooling = env.get('GITHUB_SHA', '')
    if not all(re.fullmatch('[0-9a-f]{40}', value) for value in [source, tooling]):
        raise ValueError('Product source and workflow SHA must be full commit identities')
    for name in ['GITHUB_RUN_ID', 'GITHUB_RUN_NUMBER', 'GITHUB_RUN_ATTEMPT']:
        if not re.fullmatch('[1-9][0-9]*', env.get(name, '')):
            raise ValueError('Missing positive producer identity: ' + name)
    if env.get('VERIFY_ONLY') == 'true':
        import sys
        sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'releases'))
        from macos_verify_authorization import authorize
        if authorize(env) != source:
            raise ValueError('Verify source mismatch')
    else:
        subprocess.run(['git', 'merge-base', '--is-ancestor', source, tooling], check=True)
    actual = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if actual != tooling:
        raise ValueError('Bridge checkout differs from trusted workflow SHA')
    return source, tooling, str(5000 + int(env['GITHUB_RUN_NUMBER']))


def accepted_run(runs, tooling, label):
    matches = [r for r in runs if r.get('path') == WORKFLOW
               and r.get('head_sha') == tooling and r.get('head_branch') == 'main'
               and r.get('event') == 'workflow_dispatch' and r.get('display_title') == label]
    if len(matches) > 1:
        raise ValueError('Ambiguous hosted shipping producer; no retry')
    return matches[0] if matches else None


def prepare_verify_mirror(api, source, tooling, env):
    import sys
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'releases'))
    from macos_verify_authorization import authorize, mirror_ref, evidence_bytes, REPO
    if authorize(env) != source:
        raise ValueError('Live bridge source admission changed')
    producer, number = env['GITHUB_RUN_ID'], env['VERIFY_PR']
    ref = mirror_ref(number, source, producer)
    # Bare transfer: never check out or execute product code with bridge credentials.
    with tempfile.TemporaryDirectory(prefix='okilum-verify-mirror-') as tmp:
        subprocess.run(['git', 'init', '--bare', '-q', tmp], check=True)
        subprocess.run(['git', '-C', tmp, 'fetch', '--no-tags', str(Path.cwd()), source], check=True)
        push_env = {**os.environ, 'GIT_CONFIG_COUNT': '1',
                    'GIT_CONFIG_KEY_0': 'http.extraHeader',
                    'GIT_CONFIG_VALUE_0': 'Authorization: Basic ' + base64.b64encode(
                        ('x-access-token:' + api.token).encode()).decode()}
        subprocess.run(['git', '-C', tmp, 'push',
                        '--force-with-lease=refs/heads/' + ref + ':',
                        'https://github.com/' + REPO + '.git', source + ':refs/heads/' + ref],
                       env=push_env, check=True, capture_output=True, timeout=120)
    evidence = {'schema': 1, 'repository': REPO, 'source': source,
                'pr': number, 'producer': producer, 'attempt': '1', 'tooling': tooling,
                'ref': ref, 'fast': 'success', 'issued': datetime.now(timezone.utc).isoformat()}
    # Revalidate after transfer; a changed remote PR must not gain an attestation.
    if authorize(env) != source:
        raise ValueError('Live admission changed while mirroring')
    # GitHub binds workflow dispatch to the protected App actor. Bind its exact
    # admission payload to the source with a write-once annotated tag using the
    # App's EXISTING contents permission; never require/grant status permission.
    name = 'okilum-verify-admission/' + number + '-' + source + '-' + producer
    tag = api.request('git/tags', 'POST', {'tag': name,
        'message': evidence_bytes(evidence).decode(), 'object': source, 'type': 'commit'})
    api.request('git/refs', 'POST', {'ref': 'refs/tags/' + name, 'sha': tag['sha']})
    return evidence_bytes(evidence).decode()


def main():
    source, tooling, build = identity(os.environ)
    verify_only = os.environ.get("VERIFY_ONLY") == "true"
    api = bridge.GitHub()
    api.log_quota()
    remote = read(api, 'git/ref/heads/main')
    # Push and mirror run independently. Wait only for the trusted mirror,
    # accepting newer tooling solely when it is also on Forgejo main. A push
    # beta starts together with github-mirror, which shares the capacity-1
    # publish runner with release selection and publication.
    mirror_deadline = time.monotonic() + MIRROR_WAIT_SECONDS
    while remote['object']['sha'] != tooling:
        candidate = remote['object']['sha']
        if not re.fullmatch('[0-9a-f]{40}', candidate):
            raise ValueError('Invalid mirror main identity')
        subprocess.run(['git', 'fetch', '--no-tags', 'origin', 'refs/heads/main:refs/remotes/origin/main'], check=True)
        trusted = subprocess.run(['git', 'merge-base', '--is-ancestor', candidate, 'origin/main']).returncode == 0
        contains_source = subprocess.run(['git', 'merge-base', '--is-ancestor', source, candidate]).returncode == 0
        if trusted and (verify_only or contains_source):
            tooling = candidate
            break
        if time.monotonic() >= mirror_deadline:
            raise ValueError('Mirror has not reached the trusted product source; no dispatch')
        time.sleep(5)
        remote = read(api, 'git/ref/heads/main')
    label = f"Mac shipping Forgejo {os.environ['GITHUB_RUN_ID']} attempt {os.environ['GITHUB_RUN_ATTEMPT']}"
    data = {'ref': 'main', 'inputs': {'source': source, 'build': build,
            'producer': os.environ['GITHUB_RUN_ID'], 'producer_number': os.environ['GITHUB_RUN_NUMBER'], 'attempt': os.environ['GITHUB_RUN_ATTEMPT'],
            'signing_identity': os.environ['OKILUM_SIGNING_IDENTITY'],
            'notary_profile': os.environ['OKILUM_NOTARY_PROFILE']}}
    if verify_only:
        data['inputs'].update(verify_only='true', verify_pr=os.environ['VERIFY_PR'],
                              verify_evidence=prepare_verify_mirror(api, source, tooling, os.environ))
    # Exactly one POST. Ambiguous response or failure is not retried.
    api.request('actions/workflows/forgejo-macos-shipping.yml/dispatches', 'POST', data)
    print(json.dumps({'event': 'hosted_dispatch_accepted', 'source': source,
                      'workflow_sha': tooling, 'build': build, 'producer': label}), flush=True)
    deadline = time.monotonic() + 100 * 60
    while time.monotonic() < deadline:
        runs = read(api, 'actions/workflows/forgejo-macos-shipping.yml/runs?event=workflow_dispatch&per_page=100')['workflow_runs']
        run = accepted_run(runs, tooling, label)
        if run:
            print('Hosted shipping artifact run: ' + run['html_url'], flush=True)
            if run['status'] == 'completed':
                if run['conclusion'] != 'success' or run.get('run_attempt') != 1:
                    raise ValueError('Hosted artifact failed or was rerun; no fallback')
                jobs = read(api, f"actions/runs/{run['id']}/jobs?per_page=100")['jobs']
                valid = [j for j in jobs if j['name'] == 'shipping' and j['conclusion'] == 'success'
                         and any(s['name'] == STEP and s['conclusion'] == 'success' for s in j['steps'])]
                if len(jobs) != 1 or len(valid) != 1:
                    raise ValueError('Required signing/notarization job did not execute')
                artifacts = read(api, f"actions/runs/{run['id']}/artifacts?per_page=100")['artifacts']
                expected = f"{'macos-verify' if verify_only else 'macos-shipping'}-{source}-{build}"
                matches = [a for a in artifacts if a['name'] == expected and not a['expired']
                           and re.fullmatch('sha256:[0-9a-f]{64}', a.get('digest', ''))]
                if len(matches) != 1:
                    raise ValueError('Missing, ambiguous or undigested shipping artifact')
                print(json.dumps({'event': 'signed_artifact_available', 'source': source,
                                  'workflow_sha': tooling, 'build': build, 'run': run['html_url'],
                                  'artifact': matches[0], 'publication': 'NONE; QA download only'}))
                if beta_publication(os.environ, source):
                    import sys
                    sys.path.insert(0, str(Path(__file__).parents[1] / 'releases'))
                    import appimage_artifact
                    import macos_hosted_handoff
                    from sparkle_sign import sign_archive
                    destination = Path(os.environ['RUNNER_TEMP']) / 'publish'
                    data = appimage_artifact.download(api.token, matches[0]['id'])
                    metadata = macos_hosted_handoff.stage(data, matches[0], source, tooling,
                        build, os.environ['GITHUB_RUN_ID'], run['id'], destination)
                    metadata['SIGNATURE'] = sign_archive(destination / metadata['archive'],
                        os.environ['SPARKLE_ED_PRIVATE_KEY'])
                    (destination / 'release.json').write_text(json.dumps(metadata))
                    print('Hosted signed Mac payload ready for protected beta publication')
                return
        time.sleep(bridge.poll_delay())
    raise TimeoutError('Hosted shipping deadline; no cancel/retry/fallback')


if __name__ == '__main__':
    main()
