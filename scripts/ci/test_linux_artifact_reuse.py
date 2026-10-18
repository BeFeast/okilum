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
import time
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

    def request(self, path, method='GET'):
        self.calls.append((method, path))
        if path.startswith('actions/runs?'):
            return {'workflow_runs': list(self.runs.values())}
        run_id = int(path.split('/')[2])
        if '/jobs?' in path:
            return {'jobs': [self.jobs[run_id]]}
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
                    return str(int(time.time())) + '\n'
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
                        created = linux.produce(store, SHA, root / f'request{run_id}', run_id=run_id)
                        if created:
                            store.accept(run_id, root / f'request{run_id}')
                        else:
                            store.runs[run_id].update(status='completed', conclusion='success')
                except Exception as error:
                    errors.append(error)

            previous = Path.cwd()
            os.chdir(root)
            try:
                with patch.dict(os.environ, PATH=str(stub) + os.pathsep + os.environ['PATH'], GITHUB_RUN_ATTEMPT='1'), \
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
                # Restart cannot bless a local file that drifted after verification.
                (root / 'qa/payload/okilum').write_bytes(b'tampered')
                self.assertEqual(linux.main('resolve'), 1)
                self.assertEqual(download.call_count, 1)

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

    def test_old_cold_identity_cannot_rebuild_after_hosted_history_retention(self):
        with patch.object(linux.subprocess, 'check_output', return_value='999900\n'):
            linux.admit_new_source(SHA, clock=lambda: 1000000)
        for committed in (1000000 - 3 * 86400, 1000000 + 301):
            with patch.object(linux.subprocess, 'check_output', return_value=str(committed)), \
                    self.assertRaisesRegex(ValueError, 'explicit recovery'):
                linux.admit_new_source(SHA, clock=lambda: 1000000)

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
