#!/usr/bin/env python3
"""Fail closed before downloading: accepted, successful, exact-source producers."""
import json
import os
import re
import subprocess


def api(route):
    return json.loads(subprocess.check_output(['gh', 'api', route], text=True))


repository = os.environ['GITHUB_REPOSITORY']
# Preserved intended N, accepted only if its producer finished successfully.
# The pre-merge push route uses the exact new source's existing producer.
old_run = os.environ.get('OLD_RUN') or '39764166082'
old_source = os.environ.get('OLD_SOURCE') or '3c2532e39b328f35d03ea962a4a0c1c14f285b59'
new_source = os.environ['NEW_SOURCE']
new_run = os.environ.get('NEW_RUN')
if not new_run:
    assert re.fullmatch('[0-9a-f]{40}', new_source)
    candidates = api(f'repos/{repository}/actions/workflows/forgejo-linux.yml/runs?head_sha={new_source}&per_page=100')['workflow_runs']
    candidates = [r for r in candidates if r['head_sha'] == new_source
                  and r['head_branch'].startswith('forgejo-linux-pr/')]
    assert len(candidates) == 1, 'Require exactly one existing source producer; never dispatch a duplicate'
    new_run = str(candidates[0]['id'])
values = dict(OLD_RUN=old_run, OLD_SOURCE=old_source, NEW_RUN=new_run, NEW_SOURCE=new_source)
for name in ['OLD', 'NEW']:
    run_id = values[name + '_RUN']
    source = values[name + '_SOURCE']
    assert run_id.isdecimal() and re.fullmatch('[0-9a-f]{40}', source)
    run = api(f'repos/{repository}/actions/runs/{run_id}')
    assert run['head_sha'] == source
    assert run['status'] == 'completed' and run['conclusion'] == 'success', run
    assert run['path'] == '.github/workflows/forgejo-linux.yml'
    assert run['head_branch'].startswith('forgejo-linux-pr/')
    artifacts = api(f'repos/{repository}/actions/runs/{run_id}/artifacts?per_page=100')['artifacts']
    matches = [a for a in artifacts if a['name'] == 'linux-appimage-' + source and not a['expired']]
    assert len(matches) == 1, matches
assert old_run != new_run
assert old_source != new_source
with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
    for key, value in values.items():
        output.write(f'{key.lower()}={value}\n')
