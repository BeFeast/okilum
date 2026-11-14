#!/usr/bin/env python3
"""Build the Windows beta on GitHub and stage it for Forgejo publication (#2927).

Okilum compiles nothing on baldr (Oleg 14.11). The Forgejo `release` job of
windows-diagnostic.yml is a `bridge` waiter. As for the Arch beta, orchestration
always comes from the current workflow ("tooling") commit and only the product
source is pinned: an explicit older main commit (#1040) or a pinned retry
predates this handoff. The waiter pushes the tooling commit to a write-once
GitHub ref (the product source is its ancestor, so it is reachable there) and
dispatches .github/workflows/forgejo-windows-release.yml on that ref. The
dispatch names the product source, the Forgejo build number (5000 + run
number), the mode and the Authenticode policy the build embeds. The hosted job
checks out the tooling and the product separately, cross-compiles the product
and, unless the Forgejo signer packs, packs it with Velopack. The waiter accepts
exactly one run of its ref, proven by that attempt's jobs, and only an artifact
uploaded by that attempt with a matching sha256 digest. It then puts
`windows-dist/` and `windows-release/` exactly where the former local build put
them. Signing, the R2 upload and publication stay on Forgejo, unchanged.

Modes: `pack` (no delta), `delta` (previous beta package from the public feed,
as `publish.py prepare` read it from R2), `payload` (the windows-signer runner
signs and packs).

    windows_handoff.py inputs --tooling DIR --product DIR   (GitHub, checks the dispatch)
    windows_handoff.py previous --directory DIR             (GitHub, public-feed delta base)
    windows_handoff.py record --output FILE                 (GitHub, artifact identity)
    windows_handoff.py run --source SHA --tooling SHA --build N --signed BOOL --archive BOOL \
        --directory target --budget S                       (Forgejo bridge waiter)
"""
import argparse
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zipfile

HERE = Path(__file__).resolve().parent
WORKFLOW = '.github/workflows/forgejo-windows-release.yml'
JOB = 'windows-beta'
BUILD_STEP = 'Cross-compile Reader and install packager'
PACK_STEP = 'Package Windows installer and update feed on Linux'
ARTIFACT = 'windows-beta'
HANDOFF = 'handoff.json'
SOURCE_FILE = 'release-source.txt'
MODES = ('pack', 'delta', 'payload')
PREFIX = 'forgejo-windows-beta-src'
# The hosted job's 45 min ceiling + 2 min, so GitHub's own deadline fires first.
RUN_TIMEOUT = 47 * 60
SHA = re.compile(r'[0-9a-f]{40}')
POSITIVE = re.compile(r'[1-9][0-9]*')
INVOCATION = re.compile(r'[0-9a-f]{32}')
CHANNEL = json.loads((HERE.parent / 'windows/channel.json').read_text())

_transport = importlib.util.spec_from_file_location('windows_transport', HERE.parent / 'ci/github-macos.py')
transport = importlib.util.module_from_spec(_transport)
_transport.loader.exec_module(transport)
_artifact = importlib.util.spec_from_file_location('windows_artifact_download', HERE / 'appimage_artifact.py')
artifact = importlib.util.module_from_spec(_artifact)
_artifact.loader.exec_module(artifact)


def check_inputs(env):
    """The dispatch inputs the hosted job builds from; anything else fails before Cargo."""
    values = {name: env.get(name, '') for name in
              ('SOURCE', 'BUILD', 'MODE', 'PRODUCER', 'ATTEMPT', 'INVOCATION', 'GITHUB_SHA')}
    if not SHA.fullmatch(values['SOURCE']):
        raise ValueError('source must be a full commit SHA')
    if not SHA.fullmatch(values['GITHUB_SHA']):
        raise ValueError('the tooling commit (GITHUB_SHA) must be a full commit SHA')
    for name in ('BUILD', 'PRODUCER', 'ATTEMPT'):
        if not POSITIVE.fullmatch(values[name]):
            raise ValueError(f'{name.lower()} must be a positive integer')
    if values['MODE'] not in MODES:
        raise ValueError(f'mode must be one of {", ".join(MODES)}')
    if not INVOCATION.fullmatch(values['INVOCATION']):
        raise ValueError('invocation must be 32 lowercase hex digits')
    return values


def identity(env):
    values = check_inputs(env)
    return {'source': values['SOURCE'], 'tooling': values['GITHUB_SHA'], 'build': int(values['BUILD']),
            'mode': values['MODE'], 'producer': values['PRODUCER'], 'attempt': values['ATTEMPT'],
            'invocation': values['INVOCATION']}


def head(directory):
    return subprocess.check_output(['git', '-C', str(directory), 'rev-parse', 'HEAD'], text=True).strip()


def check_checkouts(env, tooling, product, head=head):
    """The workflow tooling is the dispatched commit; the product is the requested source."""
    values = check_inputs(env)
    if head(tooling) != values['GITHUB_SHA']:
        raise ValueError('Tooling checkout is not the dispatched workflow commit')
    if head(product) != values['SOURCE']:
        raise ValueError('Product checkout is not the requested source')
    return values


class PublicFeed:
    """Read-only `store` for publish.prepare: the public root serves the R2 bucket."""

    def __init__(self, root=CHANNEL['public_root'], opener=urllib.request.urlopen):
        self.root, self.opener = root.rstrip('/'), opener

    def call(self, method, path):
        if method != 'GET':
            raise ValueError('The public feed is read-only')
        request = urllib.request.Request(f'{self.root}/{path}', headers={'User-Agent': 'okilum-windows-beta'})
        try:
            with self.opener(request, timeout=300) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            if error.code == 404:
                error.close()
                return None
            raise


def previous(directory, store=None):
    """`publish.py prepare` with the public feed instead of R2 credentials."""
    spec = importlib.util.spec_from_file_location('windows_publish', HERE.parent / 'windows/publish.py')
    publish = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(publish)
    publish.prepare(Path(directory), store or PublicFeed())


def label(producer, attempt, invocation):
    """The hosted run-name, for people; runs are matched by their write-once ref."""
    return f'Windows beta Forgejo {producer} attempt {attempt} {invocation}'


def branch_for(build, source, run_id, attempt, invocation):
    """The write-once ref; it points at the tooling commit and names the product source."""
    for value in (build, run_id, attempt):
        if not POSITIVE.fullmatch(str(value)):
            raise ValueError('Expected positive build/run/attempt identifiers')
    if not SHA.fullmatch(source) or not INVOCATION.fullmatch(invocation):
        raise ValueError('Expected a full source SHA and a 32-hex invocation')
    return f'{PREFIX}/{build}-{source}-{run_id}-{attempt}-{invocation}'


def mode_for(signed, archive, env):
    """The former `release` job's step conditions, decided before the hosted build.

    Signed: the windows-signer runner packs. A main beta is packed with a delta
    against the previous beta package; an explicit older commit (#1040) is
    archived, not appended to the delta chain.
    """
    if signed:
        return 'payload'
    if env.get('GITHUB_REF') == 'refs/heads/main' and env.get('GITHUB_EVENT_NAME') != 'pull_request' and not archive:
        return 'delta'
    return 'pack'


def runs_of(api, branch, tooling, read):
    data = read(f'actions/workflows/{Path(WORKFLOW).name}/runs?branch={branch}&event=workflow_dispatch&per_page=100')
    return [run for run in data.get('workflow_runs', [])
            if run.get('head_branch') == branch and run.get('head_sha') == tooling
            and run.get('event') == 'workflow_dispatch' and run.get('path') == WORKFLOW]


def accepted_run(api, run, mode, read):
    """A completed, successful attempt whose own jobs prove the build (and pack)."""
    attempt = run.get('run_attempt')
    if (run.get('status') != 'completed' or run.get('conclusion') != 'success'
            or type(run.get('id')) is not int or type(attempt) is not int or attempt < 1
            or not run.get('run_started_at')):
        raise ValueError('Hosted Windows run is not an accepted completed attempt')
    jobs = read(f"actions/runs/{run['id']}/attempts/{attempt}/jobs?per_page=100")
    if jobs.get('total_count', len(jobs.get('jobs', []))) != len(jobs.get('jobs', [])):
        raise ValueError('Incomplete hosted job evidence')
    if any(job.get('run_id') != run['id'] or job.get('run_attempt') != attempt for job in jobs['jobs']):
        raise ValueError('Hosted Windows jobs belong to another run or attempt')
    for step in [BUILD_STEP] + ([PACK_STEP] if mode != 'payload' else []):
        if not transport.executed_step(jobs['jobs'], JOB, step):
            raise ValueError(f'Hosted Windows attempt {attempt} did not execute {step!r}')
    return run


def cancel(api, run, log):
    try:
        api.request(f"actions/runs/{run['id']}/cancel", 'POST')
    except transport.Unavailable as error:
        log(f"::warning::Could not cancel hosted run {run['id']}: {transport.safe_diagnostic(error, api.token)}", flush=True)


def wait(api, branch, tooling, mode, *, clock=time.monotonic, sleep=time.sleep, deadline, log=print):
    """The one dispatched run of this write-once ref; None when it did not succeed."""
    read = lambda path: transport.hosted_request(api, path, sleep=sleep)
    running_since = None
    announced = False
    while True:
        runs = runs_of(api, branch, tooling, read)
        if len(runs) > 1:
            raise ValueError(f'Expected one hosted Windows run for {branch}, found {len(runs)}')
        run = runs[0] if runs else None
        if run:
            if not announced:
                announced = True
                log(f"Hosted Windows beta run: {run.get('html_url')}", flush=True)
            if run['status'] == 'completed':
                if run.get('conclusion') != 'success':
                    log(f"Hosted Windows beta: {run.get('conclusion')} — {run.get('html_url')}", flush=True)
                    return None
                return accepted_run(api, run, mode, read)
            if run['status'] == 'in_progress' and running_since is None:
                running_since = clock()
            if running_since is not None and clock() - running_since > RUN_TIMEOUT:
                cancel(api, run, log)
                raise ValueError(f"Hosted Windows beta exceeded {RUN_TIMEOUT} s: {run.get('html_url')}")
        if clock() >= deadline:
            if run:
                cancel(api, run, log)
            raise ValueError('Hosted Windows beta did not finish within the bridge budget')
        sleep(transport.poll_delay())


def safe_name(name):
    path = PurePosixPath(name)
    if (path.is_absolute() or '..' in path.parts or '\\' in name
            or not path.parts or path.parts[0] not in ('windows-dist', 'windows-release', HANDOFF)):
        raise ValueError(f'Unexpected hosted Windows artifact entry: {name!r}')
    return path


def feed_version(data):
    feed = json.loads(data)
    versions = [tuple(map(int, a['Version'].split('.'))) for a in feed['Assets']]
    return '.'.join(map(str, max(versions)))


def unpack(data, digest, expected, directory):
    """Stage only the expected exact-source, exact-build tree; anything else fails closed."""
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', digest or ''):
        raise ValueError('Hosted Windows artifact has no digest')
    if digest != 'sha256:' + hashlib.sha256(data).hexdigest():
        raise ValueError('Hosted Windows artifact digest mismatch')
    build, mode, source = expected['build'], expected['mode'], expected['source']
    version = f'0.1.{build}'
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = [name for name in archive.namelist() if not name.endswith('/')]
        paths = {name: safe_name(name) for name in names}
        if len(set(names)) != len(names):
            raise ValueError('Duplicate hosted Windows artifact entries')
        recorded = json.loads(archive.read(HANDOFF))
        if recorded != expected:
            raise ValueError(f'Hosted Windows artifact names another build: {recorded}')
        required = {'windows-dist/okilum.exe', 'windows-dist/okilum-sync-supervisor.exe',
                    f'windows-dist/okilum-{version}-x86_64.zip'}
        release = {name for name in names if paths[name].parts[0] == 'windows-release'}
        if mode == 'payload':
            if release:
                raise ValueError('A signer payload must not carry an unsigned package')
        else:
            required |= {f'windows-release/{SOURCE_FILE}', 'windows-release/releases.beta.json',
                         'windows-release/BeFeast.Okilum-beta-Setup.exe',
                         f'windows-release/BeFeast.Okilum-{version}-beta-full.nupkg'}
        missing = sorted(required - set(names))
        if missing:
            raise ValueError(f'Hosted Windows artifact lacks {missing}')
        if mode != 'payload':
            if archive.read(f'windows-release/{SOURCE_FILE}').decode().strip() != source:
                raise ValueError('Hosted Windows package was built from another source')
            if feed_version(archive.read('windows-release/releases.beta.json')) != version:
                raise ValueError('Hosted Windows feed does not end at this build')
        directory = Path(directory)
        for name in names:
            if name == HANDOFF:
                continue
            target = directory.joinpath(*paths[name].parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.read(name))
    return sorted(name for name in names if name != HANDOFF)


def stage(api, run, expected, directory, fetch):
    items = api.request(f"actions/runs/{run['id']}/artifacts?per_page=100")
    if items.get('total_count', len(items.get('artifacts', []))) != len(items.get('artifacts', [])):
        raise ValueError('Incomplete hosted artifact inventory')
    matches = [item for item in items['artifacts'] if item.get('name') == ARTIFACT]
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous hosted Windows artifact')
    item = matches[0]
    origin = item.get('workflow_run') or {}
    # Bound to the accepted attempt: uploaded by this run of the tooling commit,
    # at or after that attempt started (an earlier attempt's artifact is refused).
    if (item.get('expired') or origin.get('id') != run['id'] or origin.get('head_sha') != expected['tooling']
            or not item.get('created_at') or item['created_at'] < run['run_started_at']):
        raise ValueError('Hosted Windows artifact does not belong to the accepted run attempt')
    return unpack(fetch(item['id']), item.get('digest'), expected, directory)


def run(args, env=os.environ, *, api=None, fetch=None, push=None, clock=time.monotonic,
        sleep=time.sleep, invocation=None):
    started = clock()
    deadline = started + args.budget
    if not SHA.fullmatch(args.source) or not SHA.fullmatch(args.tooling):
        raise ValueError('Expected full product source and tooling SHAs')
    mode = mode_for(args.signed == 'true', args.archive == 'true', env)
    api = api or transport.GitHub()
    push = push or transport.push_head
    producer, attempt = env['GITHUB_RUN_ID'], env.get('GITHUB_RUN_ATTEMPT') or '1'
    invocation = invocation or uuid.uuid4().hex
    branch = branch_for(args.build, args.source, producer, attempt, invocation)
    expected = {'source': args.source, 'tooling': args.tooling, 'build': args.build, 'mode': mode,
                'producer': producer, 'attempt': attempt, 'invocation': invocation}
    api.log_quota()
    pushed = False
    try:
        # Always the workflow commit: GitHub runs the workflow file of the
        # dispatched ref, and a historical product tree predates this handoff.
        push(branch, args.tooling, api.token)
        pushed = True
        inputs = {'source': args.source, 'build': str(args.build), 'mode': mode,
                  'producer': producer, 'attempt': attempt, 'invocation': invocation,
                  'sign_subject': env.get('OKILUM_WINDOWS_SIGN_SUBJECT', ''),
                  'sign_issuer_chain': env.get('OKILUM_WINDOWS_SIGN_ISSUER_CHAIN', '')}
        # Exactly one POST; an ambiguous failure is not retried (no duplicate builds).
        api.request(f'actions/workflows/{Path(WORKFLOW).name}/dispatches', 'POST',
                    {'ref': branch, 'inputs': inputs})
        print(json.dumps({'event': 'hosted_dispatch_accepted', 'ref': branch, 'tooling': args.tooling,
                          'source': args.source, 'build': args.build, 'mode': mode}), flush=True)
        hosted = wait(api, branch, args.tooling, mode, clock=clock, sleep=sleep, deadline=deadline)
        if hosted is None:
            return 1
        fetch = fetch or (lambda artifact_id: artifact.download(api.token, artifact_id))
        staged = stage(api, hosted, expected, args.directory, fetch)
        print(f'Staged hosted Windows build {args.build} ({mode}) from {args.source}: '
              f'{len(staged)} files, {hosted.get("html_url")}', flush=True)
        return 0
    finally:
        if pushed:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable as error:
                print(f'::warning::Temporary Windows ref cleanup failed: '
                      f'{transport.safe_diagnostic(error, api.token)}', flush=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    check = sub.add_parser('inputs')
    check.add_argument('--tooling', required=True)
    check.add_argument('--product', required=True)
    prev = sub.add_parser('previous')
    prev.add_argument('--directory', required=True)
    record = sub.add_parser('record')
    record.add_argument('--output', required=True)
    build = sub.add_parser('run')
    build.add_argument('--source', required=True)
    build.add_argument('--tooling', required=True)
    build.add_argument('--build', type=int, required=True)
    build.add_argument('--signed', choices=('true', 'false'), required=True)
    build.add_argument('--archive', choices=('true', 'false'), required=True)
    build.add_argument('--directory', required=True)
    build.add_argument('--budget', type=int, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == 'inputs':
            values = check_checkouts(os.environ, args.tooling, args.product)
            print(f"Windows beta build {values['BUILD']} ({values['MODE']}) from {values['SOURCE']}")
            return 0
        if args.command == 'previous':
            previous(args.directory)
            return 0
        if args.command == 'record':
            Path(args.output).write_text(json.dumps(identity(os.environ)))
            return 0
        if args.build <= 0 or args.budget <= 0:
            parser.error('build and budget must be positive')
        return run(args)
    except (transport.Unavailable, ValueError) as error:
        print(f'Hosted Windows beta unavailable or rejected; no fallback: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
