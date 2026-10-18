#!/usr/bin/env python3
"""Single required-CI Linux producer and exact-identity artifact consumers."""
import argparse
from datetime import datetime, timezone
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import uuid
import urllib.error
import urllib.request
import zipfile


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


transport = load('transport', 'github-macos.py')
payload = load('payload', 'package-linux-binary.py')
WORKFLOW = payload.WORKFLOW
BUILD_STEP = 'Run full Linux gate'
PRODUCE_STEP = 'Produce canonical Linux payload'


def now():
    return datetime.now(timezone.utc).isoformat()


def event(path, name, sha, **facts):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('a') as stream:
        stream.write(json.dumps({'schema': 1, 'event': name, 'observed_at': now(), 'source_sha': sha,
            'target': payload.TARGET, 'profile': 'release', 'qa_started': None, 'qa_completed': None,
            'available_verified': None, **facts}, sort_keys=True) + '\n')


def origin(branch, sha):
    match = re.fullmatch(r'forgejo-linux-pr/[1-9][0-9]*/([0-9a-f]{40})-([1-9][0-9]*)-([1-9][0-9]*)-b([1-9][0-9]*)-([0-9a-f]{32})', branch)
    payload.require(match is not None and match[1] == sha, 'Invalid originating Forgejo ref')
    return {'forgejo_run_id': int(match[2]), 'forgejo_run_attempt': int(match[3]), 'forgejo_run_number': int(match[4])}


def producer(run, sha):
    payload.require(run['head_sha'] == sha and run['event'] == 'push' and run['path'] == WORKFLOW
                    and run.get('repository', {}).get('full_name') == transport.REPOSITORY,
                    'Wrong producer repository/workflow/source')
    return dict(repository=transport.REPOSITORY, workflow=WORKFLOW, event='push', source_sha=sha,
        run_id=int(run['id']), attempt=int(run['run_attempt']), **origin(run['head_branch'], sha))


def pages(api, path, key):
    values = []
    for page in range(1, 101):
        result = transport.hosted_request(api, path + ('&' if '?' in path else '?') + f'per_page=100&page={page}')
        batch = result[key]
        values.extend(batch)
        if len(batch) < 100:
            return values
    raise transport.Unavailable('Artifact discovery exceeded pagination limit')


def jobs(api, run):
    result = pages(api, f"actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", 'jobs')
    native = [job for job in result if job['name'] == 'linux']
    payload.require(len(native) == 1, 'Missing/ambiguous Linux job')
    return native[0]


def accepted(api, run):
    payload.require(run['status'] == 'completed' and run['conclusion'] == 'success', 'Producer not successful')
    job = jobs(api, run)
    payload.require(job['conclusion'] == 'success' and all(
        len([s for s in job['steps'] if s['name'] == name and s['conclusion'] == 'success' and s.get('started_at')]) == 1
        for name in (BUILD_STEP, PRODUCE_STEP)), 'Missing executed full gate/production step')
    return job


def discover(api, sha, configuration, exclude=None):
    """No dispatch here. A missing/expired attempted payload is never a cache miss."""
    name = payload.artifact_name(sha, configuration)
    runs = pages(api, f'actions/runs?head_sha={sha}&event=push', 'workflow_runs')
    matches, active, attempted = [], [], []
    for run in runs:
        if run['id'] == exclude or run['head_sha'] != sha or run['path'] != WORKFLOW or run['event'] != 'push':
            continue
        # Legacy gates predate this producer and cannot authorize/relabel a payload.
        if '-b' not in run['head_branch']:
            continue
        p = producer(run, sha)
        if run['status'] != 'completed':
            active.append(run)
            continue
        job = jobs(api, run)
        steps = [s for s in job['steps'] if s['name'] == PRODUCE_STEP and s.get('started_at') and s.get('conclusion') != 'skipped']
        if steps:
            attempted.append(run)
        artifacts = pages(api, f"actions/runs/{run['id']}/artifacts", 'artifacts')
        artifacts = [a for a in artifacts if a['name'] == name]
        if artifacts:
            accepted(api, run)
            payload.require(len(artifacts) == 1, 'Ambiguous production artifact')
            a = artifacts[0]
            payload.require(not a['expired'] and datetime.fromisoformat(a['expires_at'].replace('Z', '+00:00')) > datetime.now(timezone.utc),
                            'Production artifact expired')
            payload.require(a['workflow_run']['id'] == run['id'] and a['workflow_run']['head_sha'] == sha
                            and re.fullmatch('sha256:[0-9a-f]{64}', a.get('digest', '')), 'Wrong artifact linkage/digest')
            matches.append((run, p, a, job))
    payload.require(len(matches) <= 1, 'Ambiguous accepted production payloads')
    if matches:
        return matches[0], active
    payload.require(not attempted, 'Production attempted but payload missing/expired/failed; explicit recovery required')
    return None, active


def download(api, match, sha, configuration, output, inspector=payload.verify_binary):
    run, p, artifact, job = match
    saved = output / 'receipt.json'
    if saved.exists():
        payload.require(not saved.is_symlink(), 'Unsafe saved receipt')
        receipt = json.loads(saved.read_text())
        m = payload.validate_receipt(receipt, sha, configuration)
        payload.require(m['producer'] == p and receipt['artifact_id'] == artifact['id']
                        and receipt['artifact_api_digest'] == artifact['digest'], 'Saved receipt differs from accepted producer')
        binary = output / 'payload/okilum'
        payload.require(binary.is_file() and not binary.is_symlink() and not binary.parent.is_symlink(), 'Unsafe saved executable')
        actual = inspector(binary, m)
        payload.require(all(actual[k] == m['elf'][k] for k in actual if k != 'runtime_libraries'), 'Saved binary facts changed')
        archive = output / (payload.artifact_name(sha, configuration) + '.tar.zst')
        if archive.exists():
            payload.require(not archive.is_symlink() and payload.digest(archive.read_bytes()) == receipt['archive_sha256'],
                            'Saved archive checksum mismatch')
            payload.require(archive.with_suffix('.zst.sha256').read_text() == receipt['archive_sha256'] + '  ' + archive.name + '\n',
                            'Saved outer checksum changed')
        return receipt
    data = api.download_artifact(artifact['id'])
    # upload-artifact v4 hashes the immutable ZIP stream; enforce the API's
    # SHA256 as well as the independent packaged tar and per-file checksums.
    payload.require(artifact['digest'] == 'sha256:' + payload.digest(data), 'Hosted artifact digest mismatch')
    name = payload.artifact_name(sha, configuration) + '.tar.zst'
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        payload.require(len(entries) == 2 and {e.filename for e in entries} == {name, name + '.sha256'}
            and all(not e.is_dir() and (e.external_attr >> 16) & 0o170000 in (0, 0o100000)
                    and e.file_size < 1024 ** 3 for e in entries), 'Unsafe/incomplete artifact ZIP')
        packed = archive.read(name)
        checksum = archive.read(name + '.sha256').decode()
    m = payload.unpack(packed, checksum, output / 'payload', sha, configuration, p, inspector)
    # Retain all three checksum layers independently in the durable receipt.
    receipt = dict(schema=1, manifest=m, artifact_id=artifact['id'], producer=p,
        artifact_api_digest=artifact['digest'], artifact_zip_sha256=payload.digest(data),
        archive_sha256=payload.digest(packed), expires_at=artifact['expires_at'],
        hosted_created=run['created_at'], hosted_job_started=job['started_at'],
        artifact_created=artifact.get('created_at'), gate_steps=job['steps'], verified_at=now(),
        qa_started=None, qa_completed=None, available_verified=None)
    (output / 'receipt.json').write_bytes(payload.encode(receipt))
    (output / name).write_bytes(packed)
    (output / (name + '.sha256')).write_text(checksum)
    return receipt


def public_read(key):
    payload.require(re.fullmatch(r'[A-Za-z0-9/._-]+', key) and '..' not in key.split('/'), 'Unsafe archive key')
    try:
        with urllib.request.urlopen('https://updates.befeast.com/' + key, timeout=60) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise transport.Unavailable('Public archive read failed') from None
    except urllib.error.URLError:
        raise transport.Unavailable('Public archive unavailable; no compiler fallback') from None


def verify_signature(package, signature):
    with tempfile.TemporaryDirectory() as temporary:
        home = Path(temporary)
        home.chmod(0o700)
        subprocess.run(['gpg', '--homedir', str(home), '--batch', '--import',
            str(payload.ROOT / 'scripts/arch/okilum-signing-key.asc')], check=True, capture_output=True)
        result = subprocess.check_output(['gpg', '--homedir', str(home), '--batch', '--status-fd', '1',
            '--verify', str(signature), str(package)], stderr=subprocess.DEVNULL, text=True)
        payload.require('[GNUPG:] VALIDSIG 7FFE9F27ECC8E253E45BAEA08AACBFB2C9E47882 ' in result,
                        'Wrong archived package signer')


def archived(sha, configuration, output, *, fetch=public_read, verify=verify_signature, inspector=payload.verify_binary):
    """Existing signed archive is durable; expired, unarchived PRs remain unavailable."""
    raw = fetch(f'okilum/releases/{sha}/linux.json')
    if raw is None:
        return None
    descriptor = json.loads(raw)
    payload.require(descriptor.get('source') == sha and descriptor.get('platform') == 'linux'
                    and type(descriptor.get('build')) is int, 'Wrong archived source')
    build = descriptor['build']
    name = f'okilum-0.1.{build}-1-x86_64.pkg.tar.zst'
    assets = descriptor.get('assets', [])
    payload.require(len(assets) == 2 and {a['name'] for a in assets} == {name, name + '.sig'}, 'Ambiguous archived payload')
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        for asset in assets:
            key = f"okilum/arch/builds/{build}/{asset['name']}"
            payload.require(asset['key'] == key, 'Wrong archived package identity')
            data = fetch(key)
            payload.require(data is not None and len(data) == asset['size']
                            and payload.digest(data) == asset['sha256'], 'Archived asset checksum mismatch')
            (root / asset['name']).write_bytes(data)
        verify(root / name, root / (name + '.sig'))
        receipt, binary = payload.package_receipt(root / name, sha, build)
    m = payload.validate_receipt(receipt, sha, configuration)
    destination = output / 'payload'
    if destination.exists():
        saved = output / 'receipt.json'
        payload.require(saved.is_file() and not saved.is_symlink() and not destination.is_symlink(), 'Incomplete saved archive payload')
        previous = payload.validate_receipt(json.loads(saved.read_text()), sha, configuration)
        payload.require(previous == m and (destination / 'okilum').is_file()
                        and not (destination / 'okilum').is_symlink()
                        and (destination / 'okilum').read_bytes() == binary, 'Saved signed-archive payload changed')
    else:
        destination.mkdir(parents=True)
        (destination / 'okilum').write_bytes(binary)
        (destination / 'okilum').chmod(0o755)
    actual = inspector(destination / 'okilum', m)
    payload.require(all(actual[k] == m['elf'][k] for k in actual if k != 'runtime_libraries'), 'Archived binary facts changed')
    (output / 'receipt.json').write_bytes(payload.encode(receipt))
    event(output / 'delivery.jsonl', 'signed_archive_reused', sha, build=build,
          original_producer=m['producer'], binary_sha256=payload.digest(binary))
    return receipt


def resolve(api, sha, output, *, configuration=None, timeout=6300, sleep=time.sleep, clock=time.monotonic,
            inspector=payload.verify_binary):
    configuration = configuration or payload.config()
    evidence = output / 'delivery.jsonl'
    event(evidence, 'artifact_requested', sha, config=configuration)
    durable = archived(sha, configuration, output, inspector=inspector)
    if durable:
        return durable
    deadline = clock() + timeout
    while True:
        match, active = discover(api, sha, configuration)
        if match:
            receipt = download(api, match, sha, configuration, output, inspector)
            event(evidence, 'artifact_verified', sha, build=receipt['manifest']['build'],
                  producer=receipt['producer'], hosted_created=receipt['hosted_created'],
                  hosted_job_started=receipt['hosted_job_started'], artifact_created=receipt['artifact_created'],
                  qa_state='waiting')
            return receipt
        payload.require(active, 'No canonical producer/artifact for this exact source; no compiler fallback')
        event(evidence, 'producer_queued_or_running', sha, runs=[dict(id=r['id'], state=r['status'],
              created_at=r['created_at'], run_started_at=r.get('run_started_at')) for r in active])
        payload.require(clock() < deadline, 'Existing producer wait deadline exceeded; not redispatched')
        sleep(20)


def provenance():
    root = payload.ROOT
    def output(*args):
        return subprocess.check_output(args, text=True).strip()
    compiler = output('rustc', '-vV')
    pin = re.search(r'^channel = "([0-9.]+)"$', (root / 'rust-toolchain.toml').read_text(), re.M)[1]
    cargo_home = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')))
    payload.require(not any((cargo_home / name).exists() for name in ('config', 'config.toml')),
                    'Untracked home Cargo configuration in canonical producer')
    payload.require(f'release: {pin}' in compiler.splitlines(), 'Compiler differs from pinned toolchain')
    return dict(rustc=compiler, cargo=output('cargo', '-V'), linker=output('mold', '--version'), toolchain=(root / 'rust-toolchain.toml').read_text(),
        cargo_lock=payload.digest((root / 'Cargo.lock').read_bytes()),
        vendor=output('git', '-C', 'vendor/gpui-component', 'rev-parse', 'HEAD'),
        patches={p.name: payload.digest(p.read_bytes()) for p in sorted((root / 'scripts/patches').glob('*.diff'))},
        cargo_config={str(p.relative_to(root)): payload.digest(p.read_bytes()) for p in sorted((root / '.cargo').rglob('*')) if p.is_file()},
        environment={'os_release': Path('/etc/os-release').read_text(), 'rustflags': payload.RUSTFLAGS,
                     'cargo_incremental': '0', 'cargo_build_jobs': '4', 'profile_overrides': 'repository defaults'})


def admit_new_source(sha, clock=time.time):
    # Run discovery is the production tombstone while hosted artifacts expire.
    # Refuse old cold identities, so eventual CI-history retention cannot turn an
    # expired/unavailable payload into permission to compile it again. Archived
    # signed payloads are resolved before this admission check, without an age limit.
    committed = int(subprocess.check_output(['git', 'show', '-s', '--format=%ct', sha], text=True).strip())
    age = clock() - committed
    payload.require(-300 <= age < 3 * 86400,
                    'No accepted payload for an old/future source; explicit recovery required')


def produce(api, sha, output, *, run_id=None):
    """Called only inside the workflow SHA/config lock, after the unchanged full gate."""
    configuration = payload.config()
    run_id = run_id or int(os.environ['GITHUB_RUN_ID'])
    current = api.request(f'actions/runs/{run_id}')
    p = producer(current, sha)
    payload.require(int(os.environ['GITHUB_RUN_ATTEMPT']) == p['attempt'], 'Stale producer attempt')
    if archived(sha, configuration, output):
        return False
    match, active = discover(api, sha, configuration, exclude=run_id)
    evidence = output / 'delivery.jsonl'
    event(evidence, 'producer_started', sha, hosted_created=current['created_at'], producer=p)
    if match:
        download(api, match, sha, configuration, output)
        event(evidence, 'production_reused', sha)
        return False
    # Workflow concurrency owns serialization. Other queued runs are followers, not
    # a reason for the lock holder to wait for a producer behind its own lock.
    # A retried attempt in this same run must never compile a second time.
    if p['attempt'] > 1:
        raise ValueError('Rerun has no accepted payload; explicit recovery required')
    payload.require(subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip() == sha,
                    'Checkout differs from production source')
    admit_new_source(sha)
    m = dict(timing={'hosted_created': current['created_at'], 'hosted_run_started': current.get('run_started_at'),
                    'hosted_job_started': jobs(api, current)['started_at']},
        schema=1, source_sha=sha, config=configuration, target=payload.TARGET,
        profile='release', features=['brain'], harness=False, gpui_test_support=False,
        build=5000 + p['forgejo_run_number'], producer=p, provenance=provenance())
    env = dict(os.environ, OKILUM_SOURCE_COMMIT=sha, OKILUM_BUILD_VERSION=str(m['build']),
        OKILUM_RELEASE_VERSION=f"0.1.{m['build']}", RUSTFLAGS=payload.RUSTFLAGS,
        CARGO_INCREMENTAL='0', CARGO_BUILD_JOBS='4')
    # Release/profile/feature environment overrides would create a different product.
    payload.require(not any(k.startswith('CARGO_PROFILE_RELEASE_') or k in ('CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET',
                            'CARGO_BUILD_RUSTFLAGS', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTC_BOOTSTRAP')
                            for k in env), 'Unexpected shipping configuration override')
    m['timing']['product_build_started'] = now()
    event(evidence, 'product_build_started', sha, build=m['build'], occurred_at=m['timing']['product_build_started'])
    subprocess.run(['cargo', 'build', '--release', '--locked', '-p', 'okilum-shell', '--bin', 'okilum'], env=env, check=True)
    m['timing']['product_build_completed'] = now()
    event(evidence, 'product_build_completed', sha, build=m['build'], occurred_at=m['timing']['product_build_completed'])
    payload.package(Path('target/release/okilum'), output, m)
    return True


def main(command='request'):
    result, branch, api = 'failure', None, None
    try:
        token = os.environ.get('MIRROR_TOKEN') or os.environ.get('GH_TOKEN')
        payload.require(token, 'Artifact API token unavailable')
        api = transport.GitHub(token)
        sha = os.environ.get('PR_HEAD_SHA') or os.environ['GITHUB_SHA']
        output = Path(os.environ.get('OKILUM_ARTIFACT_DIR', 'dist/linux-production'))
        if command == 'produce':
            created = produce(api, sha, output)
            with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
                stream.write(f'created={str(created).lower()}\n')
            return 0
        if command == 'request':
            # Exact accepted evidence also satisfies a repeated required check;
            # an existing producer is observed, never dispatched a second time.
            configuration = payload.config()
            durable = archived(sha, configuration, output)
            if durable:
                result = 'success'
                return 0
            match, active = discover(api, sha, configuration)
            if match or active:
                resolve(api, sha, output, configuration=configuration)
                result = 'success'
                return 0
            # Cold exact source: unique request metadata carries the original
            # Forgejo number; the workflow SHA lock arbitrates simultaneous requests.
            branch = transport.ref_name(os.environ.get('PR_NUMBER') or '1', sha, os.environ['GITHUB_RUN_ID'],
                os.environ.get('GITHUB_RUN_ATTEMPT') or '1', uuid.uuid4().hex)
            prefix, invocation = branch.rsplit('-', 1)
            number = os.environ['GITHUB_RUN_NUMBER']
            payload.require(re.fullmatch('[1-9][0-9]*', number), 'Missing originating Forgejo run number')
            branch = prefix.replace('forgejo-pr/', 'forgejo-linux-pr/', 1) + f'-b{number}-{invocation}'
            event(output / 'delivery.jsonl', 'gate_requested', sha, forgejo_run_id=os.environ['GITHUB_RUN_ID'],
                  forgejo_run_number=number, forgejo_started_at=os.environ.get('OKILUM_TASK_STARTED_AT'))
            transport.push_head(branch, sha, token)
            observed = []
            def record_run(run):
                state = (run['id'], run['status'], run.get('conclusion'))
                if not observed or observed[-1] != state:
                    observed.append(state)
                    event(output / 'delivery.jsonl', 'hosted_run_observed', sha, run_id=run['id'],
                          state=run['status'], hosted_created=run.get('created_at'),
                          hosted_run_started=run.get('run_started_at'))
            result, message = transport.wait_for_run(api, branch, sha, workflow=WORKFLOW,
                job_name='linux', build_step=BUILD_STEP, queue_timeout=900, run_timeout=5400, on_run=record_run)
            print(message)
            payload.require(result == 'success', 'Required full Linux gate did not pass')
        resolve(api, sha, output)
        if command == 'resolve':
            event(output / 'delivery.jsonl', 'qa_requested', sha)
        result = 'success'
    except (transport.Unavailable, ValueError, KeyError, OSError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        result = 'failure'
        print(f'Linux production unavailable; no compiler fallback: {error}')
    finally:
        if command != 'produce' and os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a') as stream:
                stream.write(f'result={result}\n')
        if branch and api:
            try:
                api.request('git/refs/heads/' + branch, 'DELETE')
            except transport.Unavailable:
                print('::warning::Temporary Linux ref cleanup failed')
    return 0 if result == 'success' else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', nargs='?', choices=['request', 'resolve', 'produce', 'record'], default='request')
    parser.add_argument('--event', choices=['task_started', 'forgejo_queued', 'qa_requested', 'qa_waiting',
        'qa_started', 'qa_completed', 'promotion_waiting_for_go', 'available_verified'])
    parser.add_argument('--facts', default='{}', help='Measured facts only; unknown times stay null')
    args = parser.parse_args()
    if args.command == 'record':
        if not args.event:
            parser.error('record requires --event')
        event(Path(os.environ.get('OKILUM_ARTIFACT_DIR', 'dist/linux-production')) / 'delivery.jsonl',
              args.event, os.environ['PR_HEAD_SHA'], **json.loads(args.facts))
    else:
        raise SystemExit(main(args.command))
