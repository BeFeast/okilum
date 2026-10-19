#!/usr/bin/env python3
"""Metadata observation only. No canonical recovery/acceptance authority."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
import urllib.error
import urllib.request
from unittest.mock import patch

sys.dont_write_bytecode = True
DIAGNOSTIC_BASE = '82e544b4fdd34d6d8f5a32b9a628d69107e2bf78'
REPORTING_CONFIG = '61e5c12a763c64169cad8b9a972fa3bccdc79c078715bcafd7b0eb1c4c02c717'
MAIN_SHA = '4cfedbf3e13f17d302e0026646885d09a41bed1f'
INDEPENDENT_ANNOTATED_CONTROL = False
SOURCE = '94fd9f959db970f79969c63703a61dbedb8df99e'
CONFIG = 'b7ddc339441ed0ac1c0bfcc4c649e541564bb33e7cc1ea2f36cb3c38520a3502'
TARGET_TAG = '2e74b9c967205d3e73dd5da352871c8185e10cbd'
REPO = 'BeFeast/okilum'
BRANCH = 'diagnostic/1431-claim-metadata-r3-20261019'
WORKFLOW = '.github/workflows/windows-native.yml'
EXPECTED_OWNER = {
    'id': 39187802433, 'run_attempt': 1, 'head_sha': SOURCE, 'event': 'push',
    'path': '.github/workflows/forgejo-linux.yml',
    'head_branch': 'forgejo-linux-pr/9249/' + SOURCE + '-14476-1-b9249-92fea86a20724bdcad015ffa3922967e',
    'repository': {'full_name': REPO},
}
# Manager-disposed scope: main+exact commit validates branch/commit context only.
# No independent annotated control exists; target404 never establishes namespace
# availability, auth/cache or historical cause. No discovery/control creation.
SOURCE_HASHES = {'github-linux.py': '8dcae8caf904348fda07d48a32cee0ff064169b23933cf4dac79d51eeebc7457', 'github-macos.py': 'e66d6751818b2a5f64f64d381907e646f5cff7f642f2296dbcbb565a4fa00e85', 'package-linux-binary.py': '6b1b5a6e8986faa6b90de06ec6fec47edc8247157dd457d13d073e22ed24e88f'}
MAX_REQUESTS = 5
TIMEOUT_SECONDS = 30
MAX_BODY_BYTES = 65536
BASE_URL = 'https://api.github.com/repos/' + REPO + '/'
HEADERS = ('X-GitHub-Request-Id', 'Date', 'Age', 'Cache-Control', 'ETag', 'Vary')


class ProbeBlocked(Exception):
    pass


def safe_headers(headers, token):
    result = {}
    for key in HEADERS:
        value = headers.get(key)
        if value is None:
            continue
        # Allowlisted header values only; discard suspicious/control/oversized values.
        if (not isinstance(value, str) or len(value) > 256
                or any(ord(c) < 32 or ord(c) > 126 for c in value)
                or (token and token in value)
                or re.search(r'authorization|cookie|bearer|token', value, re.I)):
            value = '[redacted]'
        result[key] = value
    return result


def missing_path(head):
    if not re.fullmatch(r'[0-9a-f]{40}', head):
        raise ProbeBlocked()
    # Merely a candidate until this run returns 404. It must never be created.
    return 'git/ref/tags/okilum-diagnostic-missing/claim-probe-1431-' + head


class BoundedResponse:
    """Preserve urllib response semantics while bounding JSON read size."""
    def __init__(self, response):
        self.response = response

    def __enter__(self):
        self.response.__enter__()
        return self

    def __exit__(self, *args):
        return self.response.__exit__(*args)

    def read(self):
        value = self.response.read(MAX_BODY_BYTES + 1)
        if len(value) > MAX_BODY_BYTES:
            raise ProbeBlocked()
        return value


class MetadataAPI:
    def __init__(self, linux, token, head, emit):
        if not token:
            raise ProbeBlocked()
        self.linux, self.token, self.emit = linux, token, emit
        target_ref = 'git/ref/' + linux.claim_ref(SOURCE, CONFIG)
        self.allowed = {
            'git/ref/heads/main', 'git/commits/' + MAIN_SHA, missing_path(head),
            target_ref, 'git/tags/' + TARGET_TAG,
        }
        if len(self.allowed) != MAX_REQUESTS:
            raise ProbeBlocked()
        self.used = set()
        self.count = 0
        self.last_status = None
        self.client = linux.transport.GitHub(token)

    def request(self, path, method='GET', data=None):
        if method != 'GET' or data is not None or path not in self.allowed:
            raise ProbeBlocked()
        if self.count >= MAX_REQUESTS or path in self.used:
            raise ProbeBlocked()
        self.count += 1
        self.used.add(path)
        self.last_status = None
        real_open = urllib.request.urlopen
        seen = False

        def observed_open(request, timeout):
            nonlocal seen
            if (seen or request.get_method() != 'GET' or request.data is not None
                    or request.full_url != BASE_URL + path or timeout != TIMEOUT_SECONDS):
                raise ProbeBlocked()
            seen = True
            record = {'method': 'GET', 'path': path, 'status': None,
                      'classification': 'transport_unavailable', 'headers': {}}
            try:
                # Redirects are denied before a second network request. Standard
                # urllib redirect handling otherwise risks credential forwarding.
                response = real_open(request, timeout=timeout)
                self.last_status = response.status
                record.update(status=response.status,
                              classification='http_success' if response.status == 200 else 'http_unexpected',
                              headers=safe_headers(response.headers, self.token))
                if response.status != 200 or response.geturl() != request.full_url:
                    response.close()
                    raise ProbeBlocked()
                return BoundedResponse(response)
            except urllib.error.HTTPError as error:
                self.last_status = error.code
                record.update(status=error.code, classification='http_404_not_proof_of_absence'
                              if error.code == 404 else 'http_error_fail_closed',
                              headers=safe_headers(error.headers, self.token))
                raise  # Existing transport preserves HTTPError.code and closes it.
            finally:
                self.emit(record)

        # Narrow hook around the unchanged GitHub.request, never stdout/full-body
        # interception. The request construction, singular paths, headers, timeout,
        # JSON parsing and Unavailable status propagation remain literal94fd.
        # Install only for this request. No retry/cache buster/environment changes.
        with patch.object(urllib.request, 'urlopen', observed_open):
            # The default opener is supplied by main/tests with redirects denied.
            result = self.client.request(path, method, data)
        # Bound the literal reader to the exact recorded target object. A changed
        # ref cannot authorize any additional path or a different owner.
        if path == 'git/ref/' + self.linux.claim_ref(SOURCE, CONFIG):
            require(isinstance(result, dict) and result.get('ref') == 'refs/' + self.linux.claim_ref(SOURCE, CONFIG)
                    and isinstance(result.get('object'), dict) and result['object'].get('type') == 'tag'
                    and result['object'].get('sha') == TARGET_TAG)
        if path == 'git/tags/' + TARGET_TAG:
            require(isinstance(result, dict) and result.get('sha') == TARGET_TAG)
            # Reject malformed JSON numbers before the literal reader's int() coercion.
            require_owner_numbers(json.loads(result['message'])['run'])
        return result


def require(value):
    if not value:
        raise ProbeBlocked()


def require_owner_numbers(run):
    require(isinstance(run, dict) and all(
        type(run.get(key)) is int and run[key] > 0 for key in ('id', 'run_attempt')))


def validate_known_tag(linux, tag):
    value = json.loads(tag['message'])
    require(tag['sha'] == TARGET_TAG
            and tag['tag'] == linux.claim_ref(SOURCE, CONFIG).removeprefix('tags/')
            and tag['object']['type'] == 'commit' and tag['object']['sha'] == SOURCE
            and type(value.get('schema')) is int and value['schema'] == 1
            and value.get('identity') == linux.payload.artifact_name(SOURCE, CONFIG))
    require_owner_numbers(value['run'])
    linux.producer(value['run'], SOURCE)
    require(value['run'] == EXPECTED_OWNER)


def diagnose(linux, api, head, emit):
    main = api.request('git/ref/heads/main')
    require(main['ref'] == 'refs/heads/main' and main['object']['type'] == 'commit'
            and main['object']['sha'] == MAIN_SHA)
    commit = api.request('git/commits/' + MAIN_SHA)
    require(commit['sha'] == MAIN_SHA and isinstance(commit.get('tree'), dict)
            and re.fullmatch(r'[0-9a-f]{40}', commit['tree'].get('sha', '')))
    try:
        api.request(missing_path(head))
    except linux.transport.Unavailable as error:
        require(error.status == 404)
    else:
        raise ProbeBlocked()  # Nonmissing negative control invalidates this run.
    emit({'classification': 'BRANCH_COMMIT_CONTROLS_ONLY',
          'independent_annotated_control': False, 'historical_cause': 'UNKNOWN'})
    # Literal original reader. No await/authorize/produce/resolve/artifact call.
    claim = linux.read_claim(api, SOURCE, CONFIG)
    if claim is None:
        emit({'classification': 'CURRENTREF404', 'cause': 'UNKNOWN'})
        # Isolated known-object observation only; never an annotated namespace control.
        try:
            tag = api.request('git/tags/' + TARGET_TAG)
        except linux.transport.Unavailable as error:
            if error.status != 404:
                raise
            emit({'classification': 'RELATED_KNOWN_TAG_OBJECT_404', 'cause': 'UNKNOWN',
                  'annotated_namespace_inference': False})
        else:
            validate_known_tag(linux, tag)
            emit({'classification': 'RELATED_KNOWN_TAG_OBJECT_CURRENTLY_VISIBLE',
                  'annotated_namespace_inference': False})
        result = 'CURRENTREF404/CAUSEUNKNOWN'
    else:
        # The API wrapper has already checked the exact ref and tag object SHA.
        require_owner_numbers(claim['run'])
        require(type(claim.get('schema')) is int and claim['schema'] == 1
                and claim['run'] == EXPECTED_OWNER)
        emit({'classification': 'CURRENTVISIBLE', 'scope': 'current_runtime_only'})
        result = 'CURRENTVISIBLE'
    emit({'classification': 'OBSERVATION_COMPLETE', 'observation': result,
          'independent_annotated_control': False, 'historical_cause': 'UNKNOWN',
          'canonical_recovery_proof': False, 'acceptance': 'OPEN'})


def load_linux():
    root = Path(__file__).resolve().parent
    for name, digest in SOURCE_HASHES.items():
        require(hashlib.sha256((root / name).read_bytes()).hexdigest() == digest)
    spec = importlib.util.spec_from_file_location('claim_probe_linux94fd', root / 'github-linux.py')
    linux = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(linux)
    return linux


def main():
    def emit(value):
        print(json.dumps(value, sort_keys=True), flush=True)
    try:
        require(len(sys.argv) == 1)
        require(os.environ.get('GITHUB_ACTIONS') == 'true'
                and os.environ.get('GITHUB_EVENT_NAME') == 'workflow_dispatch'
                and os.environ.get('GITHUB_REPOSITORY') == REPO
                and os.environ.get('GITHUB_REF') == 'refs/heads/' + BRANCH
                and os.environ.get('GITHUB_WORKFLOW_REF') == REPO + '/' + WORKFLOW + '@refs/heads/' + BRANCH
                and os.environ.get('GITHUB_JOB') == 'claim-metadata'
                and os.environ.get('GITHUB_RUN_ATTEMPT') == '1'
                and re.fullmatch(r'[1-9][0-9]*', os.environ.get('GITHUB_RUN_ID', ''))
                and os.environ.get('GITHUB_WORKFLOW_SHA') == os.environ.get('GITHUB_SHA')
                and not os.environ.get('MIRROR_TOKEN'))
        head = os.environ.get('GITHUB_SHA', '')
        missing_path(head)
        linux = load_linux()
        emit({'classification': 'runtime_context', 'repository': REPO, 'ref': 'refs/heads/' + BRANCH,
              'diagnostic_head': head, 'source_baseline': DIAGNOSTIC_BASE, 'original_claim_source': SOURCE,
              'workflow': WORKFLOW, 'run': os.environ['GITHUB_RUN_ID'], 'attempt': 1,
              'credential_class': 'job-scoped GITHUB_TOKEN injected as GH_TOKEN from github.token',
              'credential_context': 'current runtime only; token value never retained',
              'reporting_configuration': REPORTING_CONFIG, 'independent_annotated_control': False,
              'permissions': {'contents': 'read', 'actions': 'read'}, 'max_requests': MAX_REQUESTS})
        api = MetadataAPI(linux, os.environ.get('GH_TOKEN'), head, emit)
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *args, **kwargs):
                return None
        # Bounded replacement uses the same urllib transport plus deny redirects.
        with patch.object(urllib.request, 'urlopen', urllib.request.build_opener(NoRedirect).open):
            diagnose(linux, api, head, emit)
        return 0
    except Exception:
        # Never print arbitrary exceptions, response bodies, token or traceback.
        emit({'classification': 'INCONCLUSIVE_STOP', 'cause': 'UNKNOWN',
              'acceptance': 'OPEN', 'canonical_recovery_proof': False})
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
