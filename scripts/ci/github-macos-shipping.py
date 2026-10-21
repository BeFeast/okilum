#!/usr/bin/env python3
"""Trusted, manual Forgejo -> hosted Mac shipping-artifact bridge. No feed writes."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import time

spec = importlib.util.spec_from_file_location('macos_bridge', Path(__file__).with_name('github-macos.py'))
bridge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bridge)
WORKFLOW = '.github/workflows/forgejo-macos-shipping.yml'
STEP = 'Build, sign and notarize exact product source'


def identity(env):
    if (env.get('GITHUB_REF') != 'refs/heads/main'
            or env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'):
        raise ValueError('Hosted shipping artifacts require a manual trusted main run')
    if env.get('GITHUB_RUN_ATTEMPT') != '1':
        raise ValueError('Shipping producer reruns are forbidden')
    source = env.get('PRODUCT_SOURCE', '')
    tooling = env.get('GITHUB_SHA', '')
    if not all(re.fullmatch('[0-9a-f]{40}', value) for value in [source, tooling]):
        raise ValueError('Product source and workflow SHA must be full commit identities')
    for name in ['GITHUB_RUN_ID', 'GITHUB_RUN_NUMBER', 'GITHUB_RUN_ATTEMPT']:
        if not re.fullmatch('[1-9][0-9]*', env.get(name, '')):
            raise ValueError('Missing positive producer identity: ' + name)
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


def main():
    source, tooling, build = identity(os.environ)
    api = bridge.GitHub(os.environ['MIRROR_TOKEN'])
    remote = api.request('git/ref/heads/main')
    if remote['object']['sha'] != tooling:
        raise ValueError('Mirror main is not the reviewed workflow SHA; no dispatch')
    label = f"Mac shipping Forgejo {os.environ['GITHUB_RUN_ID']} attempt {os.environ['GITHUB_RUN_ATTEMPT']}"
    data = {'ref': 'main', 'inputs': {'source': source, 'build': build,
            'producer': os.environ['GITHUB_RUN_ID'], 'producer_number': os.environ['GITHUB_RUN_NUMBER'], 'attempt': os.environ['GITHUB_RUN_ATTEMPT'],
            'signing_identity': os.environ['OKILUM_SIGNING_IDENTITY'],
            'notary_profile': os.environ['OKILUM_NOTARY_PROFILE']}}
    # Exactly one POST. Ambiguous response or failure is not retried.
    api.request('actions/workflows/forgejo-macos-shipping.yml/dispatches', 'POST', data)
    print(json.dumps({'event': 'hosted_dispatch_accepted', 'source': source,
                      'workflow_sha': tooling, 'build': build, 'producer': label}), flush=True)
    deadline = time.monotonic() + 100 * 60
    while time.monotonic() < deadline:
        runs = api.request('actions/workflows/forgejo-macos-shipping.yml/runs?event=workflow_dispatch&per_page=100')['workflow_runs']
        run = accepted_run(runs, tooling, label)
        if run:
            print('Hosted shipping artifact run: ' + run['html_url'], flush=True)
            if run['status'] == 'completed':
                if run['conclusion'] != 'success' or run.get('run_attempt') != 1:
                    raise ValueError('Hosted artifact failed or was rerun; no fallback')
                jobs = api.request(f"actions/runs/{run['id']}/jobs?per_page=100")['jobs']
                valid = [j for j in jobs if j['name'] == 'shipping' and j['conclusion'] == 'success'
                         and any(s['name'] == STEP and s['conclusion'] == 'success' for s in j['steps'])]
                if len(jobs) != 1 or len(valid) != 1:
                    raise ValueError('Required signing/notarization job did not execute')
                artifacts = api.request(f"actions/runs/{run['id']}/artifacts?per_page=100")['artifacts']
                expected = f'macos-shipping-{source}-{build}'
                matches = [a for a in artifacts if a['name'] == expected and not a['expired']
                           and re.fullmatch('sha256:[0-9a-f]{64}', a.get('digest', ''))]
                if len(matches) != 1:
                    raise ValueError('Missing, ambiguous or undigested shipping artifact')
                print(json.dumps({'event': 'signed_artifact_available', 'source': source,
                                  'workflow_sha': tooling, 'build': build, 'run': run['html_url'],
                                  'artifact': matches[0], 'publication': 'NONE; QA download only'}))
                return
        time.sleep(20)
    raise TimeoutError('Hosted shipping deadline; no cancel/retry/fallback')


if __name__ == '__main__':
    main()
