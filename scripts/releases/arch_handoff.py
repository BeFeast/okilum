#!/usr/bin/env python3
"""Build the beta Arch package on GitHub and stage it for Forgejo publication (#2927).

Okilum compiles nothing on baldr (Oleg 14.11). The Forgejo `release` job of
linux-release.yml is a `bridge` waiter. It pushes the current workflow
("tooling") commit to a write-once GitHub ref. The ref names the Forgejo build
number (5000 + run number), the exact product source and whether the QA harness
is wanted. The hosted .github/workflows/forgejo-arch-release.yml always runs from
that tooling commit and checks the product source out separately, so a
historical `inputs.source` or pinned `hourly_source` build keeps today's
orchestration while building the old product tree with its own scripts. The
waiter then stages the accepted run's package and release-source.txt exactly
where the former local build put them. The Forgejo `arch-publication` artifact,
release-publish and its `pkgver == 0.1.<build>-1` check stay unchanged.

    arch_handoff.py ref --ref <GITHUB_REF_NAME> --sha <GITHUB_SHA>       (GitHub side)
    arch_handoff.py run --source <product sha> --tooling <workflow sha> --build <n>
                        --harness 0|1 --directory dist/arch --budget <s>
"""
import argparse
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import re
import sys
import time
import uuid
import zipfile

spec = importlib.util.spec_from_file_location('arch_transport', Path(__file__).parents[1] / 'ci/github-macos.py')
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
_artifact = importlib.util.spec_from_file_location('arch_artifact_download', Path(__file__).with_name('appimage_artifact.py'))
artifact = importlib.util.module_from_spec(_artifact)
_artifact.loader.exec_module(artifact)

WORKFLOW = '.github/workflows/forgejo-arch-release.yml'
JOB = 'arch-release'
BUILD_STEP = 'Build package as an unprivileged user'
VALIDATE_STEP = 'Validate package and install without changing MIME defaults'
ARTIFACT = 'arch-release'
SOURCE_FILE = 'release-source.txt'
# The hosted job's 45 min ceiling + 2 min, so GitHub's own deadline fires first.
RUN_TIMEOUT = 47 * 60
# (pinned, harness) -> ref namespace. "Pinned" means the product source is not
# the pushed tooling commit (historical or pinned-retry builds).
PREFIX = {
    (False, '0'): 'forgejo-arch-release-pr',
    (False, '1'): 'forgejo-arch-qa-pr',
    (True, '0'): 'forgejo-arch-pinned-pr',
    (True, '1'): 'forgejo-arch-pinned-qa-pr',
}
KIND = {prefix: key for key, prefix in PREFIX.items()}
SHA = re.compile(r'[0-9a-f]{40}')
DIGEST = re.compile(r'sha256:[0-9a-f]{64}')
REF = re.compile(r'(' + '|'.join(re.escape(p) for p in PREFIX.values()) + r')/([1-9][0-9]*)-([0-9a-f]{40})'
                 r'-[1-9][0-9]*-[1-9][0-9]*-[0-9a-f]{32}')


def package_name(build):
    return f'okilum-0.1.{build}-1-x86_64.pkg.tar.zst'


def parse_ref(ref, sha):
    """(build, harness, product source) from the write-once ref pushed at `sha`.

    An unpinned ref must name the pushed commit itself; a pinned ref must name a
    different product commit, so the two forms can never be confused.
    """
    match = REF.fullmatch(ref or '')
    if not match or not SHA.fullmatch(sha or ''):
        raise ValueError(f'Not an exact-source Arch release ref: {ref!r}')
    pinned, harness = KIND[match[1]]
    source = match[3]
    if (source != sha) != pinned:
        raise ValueError(f'Arch release ref does not match the pushed commit: {ref!r}')
    return int(match[2]), harness, source


def branch_for(build, source, tooling, harness, run_id, attempt, invocation):
    if harness not in ('0', '1'):
        raise ValueError('Harness must be 0 or 1')
    if not SHA.fullmatch(source or '') or not SHA.fullmatch(tooling or ''):
        raise ValueError('Expected full source and tooling SHAs')
    prefix = PREFIX[(source != tooling, harness)]
    return transport.ref_name(build, source, run_id, attempt, invocation).replace('forgejo-pr/', prefix + '/', 1)


def accepted_run(api, branch, tooling):
    """The one completed, successful hosted run of this write-once ref.

    A write-once ref has exactly one run; any second run (or a rerun that is not
    the attempt whose jobs prove the build and validation) fails closed.
    """
    runs = api.request(f'actions/runs?branch={branch}&head_sha={tooling}&event=push&per_page=100')
    matches = [run for run in runs.get('workflow_runs', [])
               if run.get('head_branch') == branch and run.get('head_sha') == tooling
               and run.get('event') == 'push' and run.get('path') == WORKFLOW]
    if len(matches) != 1:
        raise ValueError(f'Expected one hosted Arch run for {branch}, found {len(matches)}')
    run = matches[0]
    attempt = run.get('run_attempt')
    if (run.get('status') != 'completed' or run.get('conclusion') != 'success'
            or type(run.get('id')) is not int or type(attempt) is not int or attempt < 1
            or not run.get('run_started_at')):
        raise ValueError('Hosted Arch run is not an accepted completed attempt')
    jobs = api.request(f"actions/runs/{run['id']}/attempts/{attempt}/jobs?per_page=100").get('jobs', [])
    if any(job.get('run_id') != run['id'] or job.get('run_attempt') != attempt for job in jobs):
        raise ValueError('Hosted Arch jobs belong to another run or attempt')
    for step in (BUILD_STEP, VALIDATE_STEP):
        if not transport.executed_step(jobs, JOB, step):
            raise ValueError(f'Hosted Arch attempt {attempt} did not execute {step!r}')
    return run


def unpack(data, digest, build, source, directory):
    """Write exactly the package and its source record; anything else fails closed."""
    if not DIGEST.fullmatch(digest or ''):
        raise ValueError('Hosted Arch artifact has no valid sha256 digest')
    if digest != 'sha256:' + hashlib.sha256(data).hexdigest():
        raise ValueError('Hosted Arch artifact digest mismatch')
    expected = {package_name(build), SOURCE_FILE}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive.namelist()
        if len(names) != 2 or set(names) != expected:
            raise ValueError(f'Unexpected hosted Arch artifact entries: {sorted(names)}')
        recorded = archive.read(SOURCE_FILE).decode().strip()
        if recorded != source:
            raise ValueError('Hosted Arch package was built from another source')
        directory.mkdir(parents=True, exist_ok=True)
        for name in names:
            (directory / name).write_bytes(archive.read(name))
    return directory / package_name(build)


def stage(api, branch, tooling, source, build, directory, fetch):
    run = accepted_run(api, branch, tooling)
    items = api.request(f"actions/runs/{run['id']}/artifacts?per_page=100")
    if items.get('total_count', len(items.get('artifacts', []))) != len(items.get('artifacts', [])):
        raise ValueError('Incomplete hosted artifact inventory')
    matches = [item for item in items['artifacts'] if item.get('name') == ARTIFACT]
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous hosted Arch artifact')
    item = matches[0]
    origin = item.get('workflow_run') or {}
    # Bound to the validated attempt: uploaded by this run, at or after the
    # accepted attempt started (an artifact from an earlier attempt is refused).
    if (item.get('expired') or origin.get('id') != run['id'] or origin.get('head_sha') != tooling
            or not item.get('created_at') or item['created_at'] < run['run_started_at']):
        raise ValueError('Hosted Arch artifact does not belong to the accepted run attempt')
    return unpack(fetch(item['id']), item.get('digest'), build, source, directory)


def run(args, *, api=None, fetch=None, clock=time.monotonic):
    started = clock()
    branch = branch_for(args.build, args.source, args.tooling, args.harness, os.environ['GITHUB_RUN_ID'],
                        os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
    api = api or transport.GitHub()
    pushed = False
    try:
        api.log_quota()
        # Always the workflow commit: GitHub runs the workflow file of the pushed
        # commit, and a historical product tree predates this handoff.
        transport.push_head(branch, args.tooling, api.token)
        pushed = True
        result, message = transport.wait_for_run(
            api, branch, args.tooling, workflow=WORKFLOW, job_name=JOB, build_step=VALIDATE_STEP,
            queue_timeout=args.budget, run_timeout=RUN_TIMEOUT, deadline=started + args.budget,
            also_required=((JOB, BUILD_STEP),))
        print(message)
        if result != 'success':
            return 1
        fetch = fetch or (lambda artifact_id: artifact.download(api.token, artifact_id))
        package = stage(api, branch, args.tooling, args.source, args.build, Path(args.directory), fetch)
        print(f'Staged hosted Arch build {args.build} from {args.source}: {package}')
        return 0
    finally:
        if pushed:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable as error:
                print(f'::warning::Temporary Arch ref cleanup failed: {transport.safe_diagnostic(error, api.token)}')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    ref = sub.add_parser('ref')
    ref.add_argument('--ref', required=True)
    ref.add_argument('--sha', required=True)
    build = sub.add_parser('run')
    build.add_argument('--source', required=True)
    build.add_argument('--tooling', required=True)
    build.add_argument('--build', type=int, required=True)
    build.add_argument('--harness', choices=('0', '1'), required=True)
    build.add_argument('--directory', required=True)
    build.add_argument('--budget', type=int, required=True)
    args = parser.parse_args(argv)
    if args.command == 'ref':
        number, harness, source = parse_ref(args.ref, args.sha)
        print(f'build={number}\nharness={harness}\nsource={source}')
        return 0
    if args.build <= 0 or args.budget <= 0:
        parser.error('build and budget must be positive')
    try:
        return run(args)
    except (transport.Unavailable, ValueError) as error:
        print(f'Hosted Arch build unavailable or rejected; no fallback: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
