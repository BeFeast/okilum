#!/usr/bin/env python3
"""Build the beta Arch package on GitHub and stage it for Forgejo publication (#2927).

Okilum compiles nothing on baldr (Oleg 14.11). The Forgejo `release` job of
linux-release.yml is a `bridge` waiter. It pushes the exact source to a
write-once GitHub ref that names the Forgejo build number (5000 + run number)
and whether the QA harness is wanted. It waits for the hosted
.github/workflows/forgejo-arch-release.yml job, then stages that job's package
and release-source.txt exactly where the former local build put them. The
Forgejo `arch-publication` artifact, release-publish and its
`pkgver == 0.1.<build>-1` check stay unchanged.

    arch_handoff.py ref --ref <GITHUB_REF_NAME> --sha <GITHUB_SHA>      (GitHub side)
    arch_handoff.py run --source <sha> --build <n> --harness 0|1 --directory dist/arch --budget <s>
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
PREFIX = {'0': 'forgejo-arch-release-pr', '1': 'forgejo-arch-qa-pr'}
REF = re.compile(r'(forgejo-arch-release-pr|forgejo-arch-qa-pr)/([1-9][0-9]*)-([0-9a-f]{40})'
                 r'-[1-9][0-9]*-[1-9][0-9]*-[0-9a-f]{32}')


def package_name(build):
    return f'okilum-0.1.{build}-1-x86_64.pkg.tar.zst'


def parse_ref(ref, sha):
    """(build, harness) from the write-once ref; the ref must name this exact commit."""
    match = REF.fullmatch(ref or '')
    if not match or match[3] != sha:
        raise ValueError(f'Not an exact-source Arch release ref: {ref!r}')
    return int(match[2]), '1' if match[1] == PREFIX['1'] else '0'


def branch_for(build, source, harness, run_id, attempt, invocation):
    if harness not in PREFIX:
        raise ValueError('Harness must be 0 or 1')
    return transport.ref_name(build, source, run_id, attempt, invocation).replace(
        'forgejo-pr/', PREFIX[harness] + '/', 1)


def accepted_run(api, branch, source):
    """The single successful hosted run of this exact ref and source."""
    runs = api.request(f'actions/workflows/{Path(WORKFLOW).name}/runs?branch={branch}&event=push&per_page=10')
    matches = [run for run in runs.get('workflow_runs', [])
               if run.get('head_branch') == branch and run.get('head_sha') == source
               and run.get('path') == WORKFLOW and run.get('conclusion') == 'success']
    if len(matches) != 1:
        raise ValueError(f'Expected one accepted hosted Arch run for {branch}, found {len(matches)}')
    return matches[0]


def unpack(data, digest, build, source, directory):
    """Write exactly the package and its source record; anything else fails closed."""
    if digest and digest != 'sha256:' + hashlib.sha256(data).hexdigest():
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


def stage(api, branch, source, build, directory, fetch):
    run = accepted_run(api, branch, source)
    items = api.request(f"actions/runs/{run['id']}/artifacts?per_page=100")
    if items.get('total_count', len(items.get('artifacts', []))) != len(items.get('artifacts', [])):
        raise ValueError('Incomplete hosted artifact inventory')
    matches = [item for item in items['artifacts'] if item.get('name') == ARTIFACT and not item.get('expired')]
    if len(matches) != 1:
        raise ValueError('Missing or ambiguous hosted Arch artifact')
    return unpack(fetch(matches[0]['id']), matches[0].get('digest'), build, source, directory)


def run(args, *, api=None, fetch=None, clock=time.monotonic):
    started = clock()
    if not re.fullmatch(r'[0-9a-f]{40}', args.source):
        raise ValueError('Expected the full source SHA')
    branch = branch_for(args.build, args.source, args.harness, os.environ['GITHUB_RUN_ID'],
                        os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
    api = api or transport.GitHub()
    pushed = False
    try:
        api.log_quota()
        transport.push_head(branch, args.source, api.token)
        pushed = True
        result, message = transport.wait_for_run(
            api, branch, args.source, workflow=WORKFLOW, job_name=JOB, build_step=VALIDATE_STEP,
            queue_timeout=args.budget, run_timeout=RUN_TIMEOUT, deadline=started + args.budget,
            also_required=((JOB, BUILD_STEP),))
        print(message)
        if result != 'success':
            return 1
        fetch = fetch or (lambda artifact_id: artifact.download(api.token, artifact_id))
        package = stage(api, branch, args.source, args.build, Path(args.directory), fetch)
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
    build.add_argument('--build', type=int, required=True)
    build.add_argument('--harness', choices=('0', '1'), required=True)
    build.add_argument('--directory', required=True)
    build.add_argument('--budget', type=int, required=True)
    args = parser.parse_args(argv)
    if args.command == 'ref':
        number, harness = parse_ref(args.ref, args.sha)
        print(f'build={number}\nharness={harness}')
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
