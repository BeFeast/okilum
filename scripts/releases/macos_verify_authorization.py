"""Exact maintainer authorization from trusted main; never inferred from PR green."""
import json
from pathlib import Path
import re
import urllib.request

REPO = 'BeFeast/okilum'


def authorize(env, api=None):
    if (env.get('VERIFY_ONLY') != 'true'
            or env.get('GITHUB_EVENT_NAME') != 'workflow_dispatch'
            or env.get('GITHUB_REF') != 'refs/heads/main'
            or env.get('GITHUB_REPOSITORY') != REPO
            or env.get('GITHUB_RUN_ATTEMPT') != '1'):
        raise ValueError('Trusted first-attempt manual verify-only required')
    source, number = env.get('VERIFY_SOURCE', ''), env.get('VERIFY_PR', '')
    if not re.fullmatch('[0-9a-f]{40}', source) or not re.fullmatch('[1-9][0-9]*', number):
        raise ValueError('Explicit exact PR/source required; no main fallback')
    allowed = json.loads(Path(__file__).with_name('macos-verify-authorizations.json').read_text())
    if allowed.get(number) != source:
        raise ValueError('Exact PR head lacks maintainer authorization in trusted main')
    if api is None:
        def api(path):
            request = urllib.request.Request('https://git.oklabs.uk/api/v1/repos/' + REPO + path)
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
    pr = api('/pulls/' + number)
    if (pr['state'] != 'open' or pr['merged'] or pr['base']['ref'] != 'main'
            or pr['base']['repo']['full_name'] != REPO
            or pr['head']['repo']['full_name'] != REPO or pr['head']['sha'] != source):
        raise ValueError('Authorized PR source changed or is not internal/open')
    status = api('/commits/' + source + '/status')
    checks = [s for s in status['statuses'] if s['context'] == 'fast / check (pull_request)']
    if status['sha'] != source or len(checks) != 1 or checks[0]['status'] != 'success':
        raise ValueError('Exact authorized source requires Linux fast success')
    return source
