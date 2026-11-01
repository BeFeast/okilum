"""Exact maintainer authorization from trusted main; never inferred from PR green."""
import json
from datetime import datetime, timezone
from pathlib import Path
import re
import urllib.request

REPO = 'BeFeast/okilum'
BRIDGE_ACTOR = ('befeast-forgejo-bridge[bot]', 348822650)


def evidence_bytes(evidence):
    return json.dumps(evidence, sort_keys=True, separators=(',', ':')).encode()


def mirror_ref(number, source, producer):
    return f'okilum-verify-source/{number}-{source}-{producer}'


def hosted_evidence(env, source, number, api=None):
    """Authenticate the bridge's fresh live admission, without reaching Forgejo."""
    if (env.get('GITHUB_ACTOR'), env.get('GITHUB_ACTOR_ID')) != (BRIDGE_ACTOR[0], str(BRIDGE_ACTOR[1])):
        raise ValueError('Verify dispatch did not originate from the protected bridge App')
    evidence = json.loads(env.get('VERIFY_EVIDENCE', ''))
    producer = env.get('PRODUCER', '')
    if not re.fullmatch('[1-9][0-9]*', producer):
        raise ValueError('Missing bridge producer identity')
    expected = {'schema': 1, 'repository': REPO, 'source': source, 'pr': number,
                'producer': producer, 'attempt': '1', 'tooling': env['GITHUB_SHA'],
                'ref': mirror_ref(number, source, producer), 'fast': 'success'}
    if not isinstance(evidence, dict) or set(evidence) != set(expected) | {'issued'}:
        raise ValueError('Incomplete bridge admission evidence')
    if any(evidence[k] != v for k, v in expected.items()):
        raise ValueError('Bridge admission identity mismatch')
    issued = datetime.fromisoformat(evidence['issued'])
    age = (datetime.now(timezone.utc) - issued).total_seconds()
    if not 0 <= age <= 7200:
        raise ValueError('Bridge admission evidence stale or in the future')
    if api is None:
        def api(path):
            request = urllib.request.Request('https://api.github.com/repos/' + REPO + '/' + path,
                headers={'Authorization': 'Bearer ' + env['VERIFY_GITHUB_TOKEN'],
                         'Accept': 'application/vnd.github+json'})
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
    ref = api('git/ref/heads/' + expected['ref'])
    if ref['ref'] != 'refs/heads/' + expected['ref'] or ref['object']['sha'] != source:
        raise ValueError('Authenticated source mirror changed')
    name = 'okilum-verify-admission/' + number + '-' + source + '-' + producer
    reference = api('git/ref/tags/' + name)
    if reference['ref'] != 'refs/tags/' + name or reference['object']['type'] != 'tag':
        raise ValueError('Missing immutable bridge attestation tag')
    tag = api('git/tags/' + reference['object']['sha'])
    if (tag['sha'] != reference['object']['sha'] or tag['tag'] != name
            or tag['object']['type'] != 'commit' or tag['object']['sha'] != source
            or tag['message'] != evidence_bytes(evidence).decode()):
        raise ValueError('Invalid immutable bridge attestation')
    return source


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
    if env.get('HOSTED_VERIFY') == 'true':
        return hosted_evidence(env, source, number, api)
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
