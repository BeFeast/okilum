"""Exact-source production/reuse under fake hosted stores and a counting compiler stub."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
import zipfile

from test_linux_binary import SHA, CONFIG, binary_bytes, manifest, receipt, inspector, tar_data, package
import tarfile

spec = importlib.util.spec_from_file_location('artifact_bridge', Path(__file__).with_name('github-linux.py'))
linux = importlib.util.module_from_spec(spec)
spec.loader.exec_module(linux)


def run(run_id=1, status='completed'):
    return dict(id=run_id, run_attempt=1, head_sha=SHA, event='push', path=linux.WORKFLOW,
        repository={'full_name': 'BeFeast/okilum'},
        head_branch=f'forgejo-linux-pr/1/{SHA}-{90 + run_id}-1-b{9107 + run_id}-' + 'a' * 32,
        status=status, conclusion='success' if status == 'completed' else None,
        created_at='2026-10-18T01:00:00Z', run_started_at='2026-10-18T01:00:10Z')


def job():
    return dict(name='linux', conclusion='success', started_at='2026-10-18T01:00:10Z', steps=[
        dict(name=linux.BUILD_STEP, conclusion='success', started_at='2026-10-18T01:00:20Z'),
        dict(name=linux.PRODUCE_STEP, conclusion='success', started_at='2026-10-18T01:01:20Z')])


class Store:
    def __init__(self, runs=None):
        self.runs = {r['id']: r for r in (runs or [])}
        self.artifacts = {}
        self.data = {}
        self.jobs = {r: job() for r in self.runs}
        self.calls = []
        self.refs, self.tags = {}, {}
        self.ref_lock = threading.Lock()
        self.lose_create_response = False

    def request(self, path, method='GET', data=None):
        self.calls.append((method, path))
        if path == 'git/tags' and method == 'POST':
            key = str(len(self.tags) + 1)
            self.tags[key] = dict(tag=data['tag'], message=data['message'], object={'type': data['type'], 'sha': data['object']})
            return {'sha': key}
        if path == 'git/refs' and method == 'POST':
            with self.ref_lock:
                if data['ref'] in self.refs:
                    raise linux.transport.Unavailable('already exists', status=422)
                self.refs[data['ref']] = dict(ref=data['ref'], object={'type': 'tag', 'sha': data['sha']})
            if self.lose_create_response:
                raise linux.transport.Unavailable('response lost')
            return self.refs[data['ref']]
        if path.startswith('git/ref/'):
            value = self.refs.get('refs/' + path.removeprefix('git/ref/'))
            if value is None:
                raise linux.transport.Unavailable('absent', status=404)
            return value
        if path.startswith('git/tags/'):
            return self.tags[path.removeprefix('git/tags/')]
        if path.startswith('actions/runs?'):
            return {'workflow_runs': list(self.runs.values())}
        run_id = int(path.split('/')[2])
        if '/jobs?' in path:
            return {'jobs': [self.jobs[run_id]] if self.jobs[run_id] is not None else []}
        if '/artifacts?' in path:
            return {'artifacts': self.artifacts.get(run_id, [])}
        return self.runs[run_id]

    def accept(self, run_id, directory):
        archives = list(directory.glob('*.tar.zst'))
        assert len(archives) == 1
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as archive:
            for path in (archives[0], archives[0].with_suffix('.zst.sha256')):
                archive.writestr(path.name, path.read_bytes())
        self.data[run_id] = data.getvalue()
        self.artifacts[run_id] = [dict(id=run_id, name=linux.payload.artifact_name(SHA, CONFIG), expired=False,
            expires_at='2099-01-01T00:00:00Z', workflow_run={'id': run_id, 'head_sha': SHA},
            digest='sha256:' + package.digest(data.getvalue()), created_at='2026-10-18T01:03:00Z')]
        self.runs[run_id]['status'] = 'completed'
        self.runs[run_id]['conclusion'] = 'success'

    def download_artifact(self, artifact_id):
        return self.data[artifact_id]


def accepted_store(root):
    binary = root / 'fake-binary'
    binary.write_bytes(binary_bytes())
    package.package(binary, root / 'upload', manifest(), inspector)
    store = Store([run()])
    store.accept(1, root / 'upload')
    return store


class ReuseTests(unittest.TestCase):
    def test_two_concurrent_requests_one_counted_compile_and_no_consumer_cargo(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            stub = root / 'bin'
            stub.mkdir()
            cargo = stub / 'cargo'
            cargo.write_text('#!' + sys.executable + '\n' +
                'from pathlib import Path\nimport json,sys\n' +
                f'p=Path({str(root / "compiler-count")!r})\n' +
                'with p.open("a") as f: f.write(json.dumps(sys.argv[1:])+"\\n")\n' +
                'Path("target/release").mkdir(parents=True, exist_ok=True)\n' +
                f'Path("target/release/okilum").write_bytes({binary_bytes()!r})\n')
            cargo.chmod(0o755)
            store = Store([run(1, 'in_progress'), run(2, 'queued')])
            production_lock = threading.Lock()  # Same lock key asserted against real YAML below.
            gate_executions, errors = [], []
            first_entered, follower_requested = threading.Event(), threading.Event()
            real_package = linux.payload.package
            real_output = subprocess.check_output

            def command(args, **kwargs):
                if args == ['git', 'rev-parse', 'HEAD']:
                    return SHA + '\n'
                if args[:3] == ['git', 'show', '-s']:
                    raise AssertionError('Commit age is not production eligibility')
                return real_output(args, **kwargs)

            def request(run_id):
                try:
                    if run_id == 2:
                        follower_requested.set()
                    with production_lock:
                        if run_id == 1:
                            first_entered.set()
                            self.assertTrue(follower_requested.wait(5))
                        gate_executions.append(run_id)
                        store.runs[run_id]['status'] = 'in_progress'
                        store.jobs[run_id]['steps'][1]['status'] = 'in_progress'
                        linux.authorize_claim(store, store.runs[run_id], SHA, CONFIG)
                        reads_start = len(store.calls)
                        created = linux.produce(store, SHA, root / f'request{run_id}', run_id=run_id)
                        self.assertTrue(all(method == 'GET' for method, _ in store.calls[reads_start:]))
                        if created:
                            store.accept(run_id, root / f'request{run_id}')
                        else:
                            store.runs[run_id].update(status='completed', conclusion='success')
                except Exception as error:
                    errors.append(error)

            previous = Path.cwd()
            os.chdir(root)
            try:
                with patch.dict(os.environ, PATH=str(stub) + os.pathsep + os.environ['PATH'], GITHUB_RUN_ATTEMPT='1', RUNNER_TEMP=str(root)), \
                    patch.object(linux.payload, 'config', return_value=CONFIG), \
                    patch.object(linux, 'archived', return_value=None), \
                    patch.object(linux, 'provenance', return_value=manifest()['provenance']), \
                    patch.object(linux.subprocess, 'check_output', side_effect=command), \
                    patch.object(linux.payload, 'package', side_effect=lambda b, o, m: real_package(b, o, m, inspector)):
                    threads = [threading.Thread(target=request, args=(i,)) for i in (1, 2)]
                    threads[0].start()
                    self.assertTrue(first_entered.wait(5))
                    threads[1].start()
                    for thread in threads:
                        thread.join(10)
                        self.assertFalse(thread.is_alive())
                    self.assertEqual(errors, [])
                    linux.resolve(store, SHA, root / 'qa', configuration=CONFIG)
                    # The stub actually ran on the positive producer control; resolution
                    # and prebuilt packaging validation must leave its count unchanged.
                    calls = (root / 'compiler-count').read_text().splitlines()
                    self.assertEqual(len(calls), 1)
                    self.assertEqual(json.loads(calls[0]), ['build', '--release', '--locked', '-p', 'okilum-shell', '--bin', 'okilum'])
                    self.assertEqual(len(gate_executions), 2)
                    self.assertEqual((root / 'qa/payload/okilum').read_bytes(), binary_bytes())
                    self.assertEqual(json.loads((root / 'qa/receipt.json').read_text())['manifest']['build'], 14108)
                    events = [json.loads(line) for line in (root / 'qa/delivery.jsonl').read_text().splitlines()]
                    self.assertTrue(all(e['available_verified'] is None and e['qa_started'] is None for e in events))
            finally:
                os.chdir(previous)
        workflow = (package.ROOT / '.github/workflows/forgejo-linux.yml').read_text()
        self.assertIn('group: linux-production-v1-${{ github.sha }}-x86_64-release', workflow)
        self.assertIn('cancel-in-progress: false', workflow)

    def test_repeated_required_request_and_saved_receipt_restart_reuse_without_dispatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = accepted_store(root)
            env = dict(PR_HEAD_SHA=SHA, MIRROR_TOKEN='fixture-token', GITHUB_OUTPUT=str(root / 'output'),
                       OKILUM_ARTIFACT_DIR=str(root / 'qa'))
            with patch.dict(os.environ, env), patch.object(linux.payload, 'config', return_value=CONFIG), \
                    patch.object(linux.transport, 'GitHub', return_value=store), \
                    patch.object(linux.transport, 'push_head') as push, \
                    patch.object(linux.transport, 'wait_for_run') as wait, \
                    patch.object(linux, 'archived', return_value=None), \
                    patch.object(store, 'download_artifact', wraps=store.download_artifact) as download:
                self.assertEqual(linux.main('request'), 0)
                self.assertEqual(linux.main('request'), 0)
                self.assertEqual(download.call_count, 1)
                push.assert_not_called()
                wait.assert_not_called()
                # A legacy output missing the ZIP must independently download it.
                (root / 'qa/artifact.zip').unlink()
                self.assertEqual(linux.main('request'), 0)
                self.assertEqual(download.call_count, 2)
                # Restart cannot bless a local file that drifted after verification.
                (root / 'qa/payload/okilum').write_bytes(b'tampered')
                self.assertEqual(linux.main('resolve'), 1)
                self.assertEqual(download.call_count, 2)

    def test_read_outage_never_turns_into_a_second_dispatch(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, dict(PR_HEAD_SHA=SHA,
                MIRROR_TOKEN='fixture-token', GITHUB_OUTPUT=str(Path(tmp) / 'output'))), \
                patch.object(linux, 'archived', return_value=None), \
                patch.object(linux, 'discover', side_effect=linux.transport.Unavailable('read outage')), \
                patch.object(linux.transport, 'push_head') as push:
            self.assertEqual(linux.main('request'), 1)
            push.assert_not_called()

    def test_queued_wait_preserves_original_times_and_never_dispatches(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = accepted_store(root)
            store.runs[1]['status'] = 'queued'
            ticks = []
            def sleep(seconds):
                ticks.append(seconds)
                store.runs[1]['status'] = 'completed'
            with patch.object(linux, 'archived', return_value=None), patch.object(linux.transport, 'push_head') as push:
                r = linux.resolve(store, SHA, root / 'qa', configuration=CONFIG, sleep=sleep)
                push.assert_not_called()
            self.assertEqual(ticks, [20])
            self.assertEqual(r['hosted_created'], '2026-10-18T01:00:00Z')
            self.assertEqual(r['hosted_job_started'], '2026-10-18T01:00:10Z')
            events = [json.loads(line) for line in (root / 'qa/delivery.jsonl').read_text().splitlines()]
            queued = next(e for e in events if e['event'] == 'producer_queued_or_running')
            self.assertEqual(queued['runs'][0]['state'], 'queued')
            self.assertNotEqual(queued['observed_at'], r['hosted_created'])

    def test_missing_expired_ambiguous_wrong_producer_skipped_gate_and_corruption_fail(self):
        changes = ['missing', 'expired', 'expiry_time', 'ambiguous', 'producer', 'skipped', 'wrong_job',
                   'digest', 'omitted_digest', 'zip_path', 'attempt']
        for change in changes:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                store = accepted_store(root)
                a = store.artifacts[1][0]
                if change == 'missing':
                    store.artifacts[1] = []
                elif change == 'expired':
                    a['expired'] = True
                elif change == 'expiry_time':
                    a['expires_at'] = '2026-01-01T00:00:00Z'
                elif change == 'omitted_digest':
                    del a['digest']
                elif change == 'wrong_job':
                    store.jobs[1]['name'] = 'linux-binary'
                elif change == 'ambiguous':
                    store.artifacts[1].append(copy.deepcopy(a))
                elif change == 'producer':
                    a['workflow_run']['head_sha'] = 'b' * 40
                elif change == 'skipped':
                    store.jobs[1]['steps'][0]['conclusion'] = 'skipped'
                elif change == 'digest':
                    store.data[1] += b'corrupt'
                elif change == 'attempt':
                    store.runs[1]['run_attempt'] = 2
                elif change == 'zip_path':
                    stream = io.BytesIO()
                    with zipfile.ZipFile(stream, 'w') as zip_archive:
                        zip_archive.writestr('../outside', b'bad')
                    store.data[1] = stream.getvalue()
                    a['digest'] = 'sha256:' + package.digest(stream.getvalue())
                real_run = subprocess.run
                compiler_calls = []
                def launch(args, **kwargs):
                    if args[0] == 'cargo':
                        compiler_calls.append(args)
                        raise AssertionError('Consumer invoked cargo')
                    return real_run(args, **kwargs)
                with patch.object(linux, 'archived', return_value=None), \
                        patch.object(linux.subprocess, 'run', side_effect=launch), self.assertRaises(ValueError):
                    linux.resolve(store, SHA, root / 'qa', configuration=CONFIG)
                self.assertEqual(compiler_calls, [])
                self.assertFalse((root / 'outside').exists())

    def test_no_accepted_payload_never_compiles_even_with_another_queued_request(self):
        store = Store([run(), run(2, 'queued')])
        with self.assertRaisesRegex(ValueError, 'explicit recovery'):
            linux.discover(store, SHA, CONFIG)
        for field, bad in [('repository', {'full_name': 'fork/repo'}), ('event', 'pull_request'),
                           ('head_sha', 'b' * 40), ('path', '.github/workflows/forgejo-linux-binary.yml')]:
            r = run()
            r[field] = bad
            with self.assertRaises(ValueError):
                linux.producer(r, SHA)
        with self.assertRaises(ValueError):
            linux.origin(f'forgejo-linux-pr/1/{SHA}-91-1-' + 'a' * 32, SHA)

    def test_claim_survives_history_loss_reruns_and_owner_changes(self):
        store = Store([run(1, 'in_progress'), run(2, 'queued')])
        store.jobs[1]['steps'][1]['status'] = 'in_progress'
        self.assertIsNone(linux.read_claim(store, SHA, CONFIG))
        store.lose_create_response = True
        self.assertTrue(linux.authorize_claim(store, store.runs[1], SHA, CONFIG))
        self.assertTrue(linux.authorize_claim(store, store.runs[1], SHA, CONFIG))
        linux.await_claim(store, store.runs[1], SHA, CONFIG)
        self.assertEqual(len(store.refs), 1)
        store.runs.clear()  # Artifact/run retention cannot turn attempted into never-attempted.
        self.assertIsNotNone(linux.read_claim(store, SHA, CONFIG))
        for changed in (run(2, 'in_progress'), dict(run(1, 'in_progress'), run_attempt=2)):
            with self.assertRaisesRegex(ValueError, 'explicit recovery'):
                linux.await_claim(store, changed, SHA, CONFIG)
        with patch.object(store, 'request', side_effect=linux.transport.Unavailable('read outage', status=503)):
            with self.assertRaises(linux.transport.Unavailable):
                linux.read_claim(store, SHA, CONFIG)
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, MIRROR_TOKEN='fake',
                PR_HEAD_SHA=SHA, OKILUM_ARTIFACT_DIR=tmp), patch.object(linux.transport, 'GitHub', return_value=store), \
                patch.object(linux, 'archived', return_value=None), patch.object(linux.payload, 'config', return_value=CONFIG), \
                patch.object(linux.transport, 'push_head') as push:
            self.assertEqual(linux.main('request'), 1)
            push.assert_not_called()

    def test_claim_created_only_after_full_gate_and_executing_producer_step(self):
        store = Store([run(1, 'queued')])
        self.assertIsNone(linux.authorize_claim(store, store.runs[1], SHA, CONFIG))
        store.runs[1]['status'] = 'in_progress'
        store.jobs[1] = None  # API run state may precede job materialization.
        self.assertIsNone(linux.authorize_claim(store, store.runs[1], SHA, CONFIG))
        store.jobs[1] = job()
        self.assertIsNone(linux.authorize_claim(store, store.runs[1], SHA, CONFIG))
        store.jobs[1]['steps'][1]['status'] = 'in_progress'
        store.jobs[1]['steps'][0]['conclusion'] = 'skipped'
        with self.assertRaisesRegex(ValueError, 'executed full gate'):
            linux.authorize_claim(store, store.runs[1], SHA, CONFIG)
        self.assertEqual(store.refs, {})
        with self.assertRaisesRegex(ValueError, 'no compilation'):
            linux.await_claim(store, store.runs[1], SHA, CONFIG, timeout=0)
        self.assertTrue(all(method == 'GET' for method, _ in store.calls))

    def test_atomic_claim_competing_bridge_observers_and_conflicting_owner(self):
        store = Store([run(1, 'in_progress'), run(2, 'in_progress')])
        for j in store.jobs.values():
            j['steps'][1]['status'] = 'in_progress'
        barrier = threading.Barrier(2)
        real = store.request
        def request(path, method='GET', data=None):
            if path == 'git/refs' and method == 'POST':
                barrier.wait(5)
            return real(path, method, data)
        results, errors = [], []
        def authorize(i):
            try:
                results.append(linux.authorize_claim(store, store.runs[i], SHA, CONFIG))
            except Exception as e:
                errors.append(e)
        with patch.object(store, 'request', side_effect=request):
            threads = [threading.Thread(target=authorize, args=(i,)) for i in (1, 2)]
            for t in threads:
                t.start()
            for t in threads:
                t.join(10)
                self.assertFalse(t.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(sorted(results), [False, True])
        self.assertEqual(len(store.refs), 1)

    def test_cancelled_empty_jobs_follower_does_not_poison_success_or_active(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = accepted_store(root)
            follower = run(2)
            follower['conclusion'] = 'cancelled'
            store.runs[2], store.jobs[2] = follower, None
            match, active = linux.discover(store, SHA, CONFIG)
            self.assertEqual(match[0]['id'], 1)
            store.runs[1]['status'] = 'in_progress'
            self.assertEqual(linux.discover(store, SHA, CONFIG)[1], [store.runs[1]])
            store.runs[1]['status'] = 'completed'
            store.artifacts.clear()
            # A real cancelled production attempt still leaves an unavailable tombstone.
            store.runs[1]['conclusion'] = 'cancelled'
            with self.assertRaisesRegex(ValueError, 'explicit recovery'):
                linux.discover(store, SHA, CONFIG)
            del store.runs[1]
            for conclusion in ('success', 'failure', 'timed_out'):
                follower['conclusion'] = conclusion
                with self.assertRaisesRegex(ValueError, 'Missing/ambiguous'):
                    linux.discover(store, SHA, CONFIG)

    def test_saved_payload_and_edited_receipt_exploit_is_rejected(self):
        for change in ('binary', 'binary_and_receipt', 'all_local_archives', 'zip', 'legacy'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                store = accepted_store(root)
                match = linux.discover(store, SHA, CONFIG)[0]
                linux.download(store, match, SHA, CONFIG, root / 'qa')
                saved = root / 'qa/receipt.json'
                r = json.loads(saved.read_text())
                if change == 'zip':
                    (root / 'qa/artifact.zip').write_bytes(b'corrupt ZIP')
                else:
                    binary = root / 'qa/payload/okilum'
                    binary.write_bytes(binary_bytes() + b'edited')
                    if change != 'binary':
                        r['manifest']['binary_sha256'] = package.digest(binary.read_bytes())
                        saved.write_bytes(package.encode(r))
                    if change == 'all_local_archives':
                        archive = next((root / 'qa').glob('*.tar.zst'))
                        archive.write_bytes(b'edited tar')
                        r['archive_sha256'] = package.digest(archive.read_bytes())
                        saved.write_bytes(package.encode(r))
                    if change == 'legacy':
                        (root / 'qa/artifact.zip').unlink()
                with self.assertRaises(ValueError):
                    linux.download(store, match, SHA, CONFIG, root / 'qa')

    def test_unsigned_receipt_and_unsigned_package_require_independent_authority(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = accepted_store(root)
            match = linux.discover(store, SHA, CONFIG)[0]
            r = linux.download(store, match, SHA, CONFIG, root / 'qa')
            with patch.object(package, 'receipt_authority', return_value=linux), \
                    patch.object(linux.transport, 'GitHub', return_value=store), \
                    patch.object(linux, 'archived', return_value=None):
                self.assertEqual(package.validate_receipt(r, SHA, CONFIG), r['manifest'])
                path = root / 'unsigned.pkg.tar.zst'
                def write_package(value, binary):
                    path.write_bytes(tar_data([('usr/bin/okilum', binary, tarfile.REGTYPE),
                        ('usr/share/okilum/production-receipt.json', package.encode(value), tarfile.REGTYPE)]))
                write_package(r, binary_bytes())
                self.assertEqual(package.package_receipt(path, SHA, 14108)[1], binary_bytes())
                bad = copy.deepcopy(r)
                edited = binary_bytes() + b'edited'
                bad['manifest']['binary_sha256'] = package.digest(edited)
                with self.assertRaisesRegex(ValueError, 'independently authenticated'):
                    package.validate_receipt(bad, SHA, CONFIG)
                write_package(bad, edited)
                with self.assertRaisesRegex(ValueError, 'independently authenticated'):
                    package.package_receipt(path, SHA, 14108)

    def test_publication_entrypoint_rechecks_unsigned_receipt_before_packaged_bytes(self):
        spec = importlib.util.spec_from_file_location('transport_publication_boundary', package.ROOT / 'scripts/releases/publication.py')
        publication = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(publication)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = accepted_store(root)
            r = linux.download(store, linux.discover(store, SHA, CONFIG)[0], SHA, CONFIG, root / 'qa')
            (root / 'arch').mkdir()
            path = root / 'arch/okilum-0.1.14108-1-x86_64.pkg.tar.zst'
            def stage(value, binary):
                (root / 'arch/production-receipt.json').write_bytes(package.encode(value))
                path.write_bytes(tar_data([('usr/bin/okilum', binary, tarfile.REGTYPE),
                    ('usr/share/okilum/production-receipt.json', package.encode(value), tarfile.REGTYPE)]))
            with patch.object(publication.native_payload, 'receipt_authority', return_value=linux), \
                    patch.object(publication.archive_authority.consumer, 'public_read', return_value=None), \
                    patch.object(linux.transport, 'GitHub', return_value=store), \
                    patch.object(linux, 'archived', return_value=None):
                stage(r, binary_bytes())
                self.assertEqual(publication.production_build(root, SHA), 14108)
                edited = binary_bytes() + b'changed package'
                r['manifest']['binary_sha256'] = package.digest(edited)
                stage(r, edited)
                with self.assertRaisesRegex(ValueError, 'independently authenticated'):
                    publication.production_build(root, SHA)

    def test_never_attempted_old_or_future_source_and_crash_before_upload(self):
        for committed in (1, 9999999999):
            with self.subTest(committed=committed), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                store = Store([run(1, 'in_progress')])
                store.jobs[1]['steps'][1]['status'] = 'in_progress'
                self.assertTrue(linux.authorize_claim(store, store.runs[1], SHA, CONFIG))
                git_calls = []
                def git(args, **kw):
                    git_calls.append(args)
                    if args == ['git', 'rev-parse', 'HEAD']:
                        return SHA + '\n'
                    if args[:3] == ['git', 'show', '-s']:
                        return str(committed) + '\n'
                    raise AssertionError(args)
                def compiler(args, **kw):
                    self.assertEqual(args, ['cargo', 'build', '--release', '--locked', '-p', 'okilum-shell', '--bin', 'okilum'])
                    raise subprocess.CalledProcessError(97, args)  # Stub crash: no artifact was uploaded.
                with patch.dict(os.environ, GITHUB_RUN_ATTEMPT='1', RUNNER_TEMP=str(root)), patch.object(linux.payload, 'config', return_value=CONFIG), \
                        patch.object(linux, 'archived', return_value=None), \
                        patch.object(linux, 'provenance', return_value=manifest()['provenance']), \
                        patch.object(linux.subprocess, 'check_output', side_effect=git), \
                        patch.object(linux.subprocess, 'run', side_effect=compiler) as compile:
                    with self.assertRaises(subprocess.CalledProcessError):
                        linux.produce(store, SHA, root, run_id=1)
                    self.assertEqual(compile.call_count, 1)
                    with self.assertRaises(FileExistsError):
                        linux.produce(store, SHA, root / 'changed-output', run_id=1)
                    self.assertEqual(compile.call_count, 1)
                    self.assertEqual(git_calls, [['git', 'rev-parse', 'HEAD']] * 2)
                    store.runs[1]['run_attempt'] = 2
                    with patch.dict(os.environ, GITHUB_RUN_ATTEMPT='2'), self.assertRaisesRegex(ValueError, 'Rerun'):
                        linux.produce(store, SHA, root / 'fresh-runner', run_id=1)
                    self.assertEqual(compile.call_count, 1)
                self.assertIsNotNone(linux.read_claim(store, SHA, CONFIG))

    def test_active_request_bridge_authorizes_while_consumer_remains_read_only(self):
        for authorize in (False, True):
            with self.subTest(authorize=authorize), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                store = accepted_store(root)
                store.runs[1]['status'] = 'in_progress'
                store.jobs[1]['steps'][1]['status'] = 'in_progress'
                def complete(seconds):
                    self.assertEqual(bool(store.refs), authorize)
                    store.runs[1]['status'] = 'completed'
                with patch.object(linux, 'archived', return_value=None):
                    linux.resolve(store, SHA, root / 'qa', configuration=CONFIG, sleep=complete, authorize=authorize)
                self.assertEqual(any(method != 'GET' for method, _ in store.calls), authorize)

    def test_request_wait_callback_writes_claim_and_preserves_origin(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            store = accepted_store(root)
            original = store.runs.pop(1)
            original['status'] = 'in_progress'
            store.jobs[1]['steps'][1]['status'] = 'in_progress'
            def wait(api, branch, sha, **kwargs):
                original['head_branch'] = branch
                # Use the originating Forgejo fixture identity from the branch.
                self.assertEqual(linux.origin(branch, sha)['forgejo_run_number'], 9108)
                store.runs[1] = original
                kwargs['on_run'](original)
                self.assertEqual(linux.read_claim(store, SHA, CONFIG)['run']['id'], 1)
                # No compiler or real dispatch: use the already-created fixture ZIP.
                original['status'] = 'completed'
                return 'success', 'fixture gate passed'
            # Ref cleanup is independent of the durable tag; model it explicitly.
            request = store.request
            def api_request(path, method='GET', data=None):
                if method == 'DELETE':
                    self.assertTrue(path.startswith('git/refs/heads/forgejo-linux-pr/'))
                    return None
                return request(path, method, data)
            env = dict(PR_HEAD_SHA=SHA, MIRROR_TOKEN='fixture-token', GITHUB_RUN_ID='91', GITHUB_RUN_ATTEMPT='1',
                       GITHUB_RUN_NUMBER='9108', PR_NUMBER='1', OKILUM_ARTIFACT_DIR=str(root / 'qa'))
            with patch.dict(os.environ, env), patch.object(linux.payload, 'config', return_value=CONFIG), \
                    patch.object(linux.transport, 'GitHub', return_value=store), \
                    patch.object(linux.transport, 'wait_for_run', side_effect=wait), \
                    patch.object(linux.transport, 'push_head') as push, \
                    patch.object(store, 'request', side_effect=api_request), \
                    patch.object(linux, 'archived', return_value=None):
                self.assertEqual(linux.main('request'), 0)
                push.assert_called_once()
            self.assertEqual(len(store.refs), 1)

    def test_pagination_and_transient_reads_do_not_redispatch(self):
        class API:
            def __init__(self):
                self.calls = []
            def request(self, path):
                self.calls.append(path)
                return {'values': list(range(100)) if path.endswith('page=1') else [100]}
        api = API()
        self.assertEqual(linux.pages(api, 'items?x=1', 'values'), list(range(101)))
        self.assertEqual(len(api.calls), 2)

    def test_signed_archive_reuses_original_receipt_after_hosted_expiry(self):
        r = receipt()
        data = tar_data([('usr/bin/okilum', binary_bytes(), tarfile.REGTYPE),
            ('usr/share/okilum/production-receipt.json', package.encode(r), tarfile.REGTYPE)])
        name = 'okilum-0.1.14108-1-x86_64.pkg.tar.zst'
        objects = {f'okilum/arch/builds/14108/{name}': data,
                   f'okilum/arch/builds/14108/{name}.sig': b'fake signature'}
        descriptor = dict(platform='linux', build=14108, source=SHA, assets=[
            dict(key=k, name=k.rsplit('/', 1)[1], size=len(v), sha256=package.digest(v)) for k, v in objects.items()])
        objects[f'okilum/releases/{SHA}/linux.json'] = package.encode(descriptor)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with patch.object(linux, 'verify_signature') as verify:
                result = linux.archived(SHA, CONFIG, root / 'qa', fetch=objects.get, verify=verify)
                verify.assert_called_once()
            self.assertEqual(result, r)
            self.assertEqual((root / 'qa/payload/okilum').read_bytes(), binary_bytes())
            # A correct outer archive checksum cannot excuse stripped payload bytes.
            objects[f'okilum/arch/builds/14108/{name}'] += b'corruption'
            with self.assertRaisesRegex(ValueError, 'checksum'):
                linux.archived(SHA, CONFIG, root / 'bad', fetch=objects.get, verify=lambda *_: None)


if __name__ == '__main__':
    unittest.main()
