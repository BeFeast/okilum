"""Trust boundary and failure/retry semantics, exercised by the hosted Linux gate."""
import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
import urllib.error
from unittest.mock import Mock, patch
import zipfile

import appimage_artifact as artifact
import appimage_publish as publisher

SOURCE = 'a' * 40


class ArtifactTransport(unittest.TestCase):
    def test_transient_failure_recovers_with_bounded_backoff(self):
        response = Mock()
        response.__enter__ = Mock(return_value=response)
        response.__exit__ = Mock(return_value=False)
        response.read.return_value = b'archive'
        opener = Mock()
        opener.open.side_effect = [urllib.error.HTTPError('https://example.invalid',
            503, 'Egress is over the account limit', {}, None), response]
        with patch.object(artifact.urllib.request, 'build_opener', return_value=opener), \
                patch.object(artifact.time, 'sleep') as sleep:
            self.assertEqual(artifact.download('test-token', 29), b'archive')
        self.assertEqual(opener.open.call_count, 2)
        sleep.assert_called_once_with(1)

    def test_exhausted_transport_is_infrastructure_failure_never_success(self):
        for code in [429, 502, 503, 504]:
            opener = Mock()
            opener.open.side_effect = [urllib.error.HTTPError(
                'https://example.invalid', code, 'unavailable', {}, None) for _ in range(3)]
            with self.subTest(code=code), \
                    patch.object(artifact.urllib.request, 'build_opener', return_value=opener), \
                    patch.object(artifact.time, 'sleep') as sleep:
                with self.assertRaisesRegex(artifact.ArtifactTransportUnavailable,
                                            'APPIMAGE_INFRA_UNAVAILABLE'):
                    artifact.download('test-token', 29)
                self.assertEqual(opener.open.call_count, 3)
                self.assertEqual([call.args[0] for call in sleep.call_args_list], [1, 2])

    def test_other_http_failures_are_not_retried(self):
        for code in [401, 403, 404, 422]:
            error = urllib.error.HTTPError('https://example.invalid', code, 'denied', {}, None)
            opener = Mock(); opener.open.side_effect = error
            with self.subTest(code=code), \
                    patch.object(artifact.urllib.request, 'build_opener', return_value=opener), \
                    patch.object(artifact.time, 'sleep') as sleep:
                with self.assertRaises(urllib.error.HTTPError):
                    artifact.download('test-token', 29)
                opener.open.assert_called_once()
                sleep.assert_not_called()


def evidence():
    run = {'id': 17, 'run_number': 23, 'run_attempt': 1, 'repository': {'full_name': artifact.REPOSITORY},
           'head_sha': SOURCE, 'head_branch': 'main', 'event': 'push', 'path': artifact.WORKFLOW,
           'status': 'completed', 'conclusion': 'success'}
    jobs = [{'id': i, 'name': name, 'run_id': 17, 'head_sha': SOURCE, 'status': 'completed',
             'conclusion': 'success', 'steps': [{'name': step, 'status': 'completed', 'conclusion': 'success'}]}
            for i, (name, step) in enumerate([('appimage', artifact.SMOKE), ('linux', 'Run full Linux gate')])]
    image = b'original embedded 5023 AppImage'
    provenance = {'source_sha': SOURCE, 'artifact': artifact.IMAGE, 'sha256': publisher.digest(image),
                  'build_version': '5023', 'release_version': '0.1.5023', 'signed': False,
                  'workflow_run': '17', 'workflow_attempt': '1'}
    archive = io.BytesIO()
    with zipfile.ZipFile(archive, 'w') as z:
        z.writestr(artifact.IMAGE, image)
        z.writestr('provenance.json', json.dumps(provenance))
        z.writestr('SHA256SUMS', publisher.digest(image) + '  ' + artifact.IMAGE + '\n')
        z.writestr('bundled-libraries.txt', '')
    data = archive.getvalue()
    item = {'id': 29, 'name': f'linux-appimage-{SOURCE}', 'expired': False,
            'workflow_run': {'id': 17, 'head_sha': SOURCE}, 'digest': 'sha256:' + publisher.digest(data)}
    return {'run': run, 'jobs': jobs, 'artifact': item}, data, image, provenance


class Resolver(unittest.TestCase):
    def test_only_exact_repository_workflow_source_first_attempt_and_executed_gates(self):
        receipt, _, _, _ = evidence()
        run, jobs = receipt['run'], receipt['jobs']
        artifact.accepted(run, jobs, SOURCE)
        for change in [{'repository': {'full_name': 'other/repo'}}, {'head_sha': 'b' * 40},
                       {'path': '.github/workflows/other.yml'}, {'event': 'pull_request'},
                       {'status': 'in_progress'}, {'conclusion': 'failure'}, {'run_attempt': 2},
                       {'head_branch': 'feature'}]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                artifact.accepted({**run, **change}, jobs, SOURCE)
        for index in [0, 1]:
            for field, value in [('conclusion', 'skipped'), ('steps', []), ('head_sha', 'b' * 40), ('run_id', 99)]:
                broken = copy.deepcopy(jobs); broken[index][field] = value
                with self.subTest(field=field, index=index), self.assertRaises(ValueError):
                    artifact.accepted(run, broken, SOURCE)

    def test_stage_digest_and_identity_fail_closed_without_dispatch(self):
        receipt, data, _, _ = evidence()
        def api_for(item=None, runs=None):
            api = Mock()
            api.request.side_effect = [{'workflow_runs': runs if runs is not None else [receipt['run']]},
                                       receipt['run'], {'jobs': receipt['jobs']},
                                       {'artifacts': [item or receipt['artifact']]}, receipt['run']]
            return api
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact.stage(api_for(), SOURCE, root / 'accepted', lambda _: data)
            artifact.unpack(root / 'accepted', root / 'unpacked', SOURCE)
            for changes in [{'expired': True}, {'digest': 'sha256:' + '0' * 64},
                            {'workflow_run': {'id': 18, 'head_sha': SOURCE}}]:
                with self.assertRaises(ValueError):
                    artifact.stage(api_for({**receipt['artifact'], **changes}), SOURCE, root / 'bad', lambda _: data)
            for runs in [[], [receipt['run'], receipt['run']]]:
                with self.assertRaises(ValueError):
                    artifact.stage(api_for(runs=runs), SOURCE, root / 'missing', lambda _: data)
            (root / 'accepted/artifact.zip').write_bytes(data + b'tampered')
            with self.assertRaises(ValueError):
                artifact.unpack(root / 'accepted', root / 'tampered', SOURCE)


class Store:
    def __init__(self):
        self.objects = {}; self.writes = []; self.fail = None

    def call(self, method, key):
        return self.objects.get(key)

    def put(self, key, data, *metadata):
        if key == self.fail:
            raise RuntimeError('interrupted upload')
        self.objects[key] = data
        self.writes.append((key, metadata))


class Key:
    fingerprint = publisher.FINGERPRINT

    def sign(self, path):
        Path(str(path) + '.sig').write_bytes(b'sig:' + hashlib.sha256(path.read_bytes()).digest())

    def verify(self, path):
        if Path(str(path) + '.sig').read_bytes() != b'sig:' + hashlib.sha256(path.read_bytes()).digest():
            raise ValueError('signature rejected')


class Publication(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(); self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / 'input'; self.directory.mkdir()
        self.receipt, _, self.image, self.provenance = evidence()
        (self.directory / artifact.IMAGE).write_bytes(self.image)
        (self.directory / 'provenance.json').write_text(json.dumps(self.provenance))
        self.original = (self.directory / 'provenance.json').read_bytes()
        self.store = Store(); self.key = Key()
        self.client = Mock()
        self.range = patch.object(publisher.release_notes, 'merged_items', return_value=[])
        self.range.start(); self.addCleanup(self.range.stop)

    def publish(self, **kwargs):
        return publisher.publish(self.store, self.key, self.root, self.directory, SOURCE, self.receipt, 9999, client=self.client, **kwargs)

    def test_same_bytes_version_separate_descriptor_and_commit_last(self):
        descriptor = self.publish()
        self.assertEqual((descriptor['build'], descriptor['publication_build']), (5023, 9999))
        prefix = 'okilum/linux/builds/5023'
        self.assertEqual(self.store.objects[prefix + '/' + artifact.IMAGE], self.image)
        self.assertEqual(self.store.objects[prefix + '/provenance.json'], self.original)
        self.assertEqual(self.store.writes[-1][0], 'okilum/linux/beta/latest.json')
        self.assertTrue(all(metadata[-1] == 'no-cache' for key, metadata in self.store.writes if '/beta/' in key))
        self.assertFalse(any('/arch/' in key or '/stable/' in key for key in self.store.objects))
        notes = json.loads(self.store.objects[publisher.release_notes.key(SOURCE, 'linux-appimage', 'beta')])
        self.assertEqual((notes['build'], notes['previous_build']), (5023, None))
        # Retry must use existing verified signatures, including after a partial upload.
        with patch.object(self.key, 'sign', side_effect=AssertionError('must reuse existing signature')):
            self.assertEqual(self.publish(), descriptor)

    def test_notes_use_appimage_predecessor_and_retry_keeps_original_range(self):
        previous = {'build': 5022, 'source': 'b' * 40,
                    'platform': 'linux-appimage', 'channel': 'beta'}
        self.store.objects['okilum/linux/beta/latest.json'] = json.dumps(previous).encode()
        self.store.objects['okilum/arch/beta/x86_64/latest.json'] = json.dumps(
            {'build': 9998, 'source': 'c' * 40}).encode()
        self.publish()
        path = publisher.release_notes.key(SOURCE, 'linux-appimage', 'beta')
        raw = self.store.objects[path]
        notes = json.loads(raw)
        self.assertEqual((notes['build'], notes['previous_build'], notes['previous_source']),
                         (5023, 5022, 'b' * 40))
        self.publish()
        self.assertEqual(self.store.objects[path], raw)
        names = [key for key, _ in self.store.writes]
        self.assertLess(names.index(path), names.index('okilum/linux/beta/latest.json'))

    def test_next_publication_reads_actual_publisher_descriptor_as_predecessor(self):
        first = self.publish()
        source = 'b' * 40
        receipt = copy.deepcopy(self.receipt)
        receipt['run'].update(run_number=24, head_sha=source)
        provenance = {**self.provenance, 'source_sha': source,
                      'build_version': '5024', 'release_version': '0.1.5024'}
        (self.directory / 'provenance.json').write_text(json.dumps(provenance))
        second = publisher.publish(self.store, self.key, self.root, self.directory,
                                   source, receipt, 10000, client=self.client)
        self.assertEqual((first['platform'], first['channel'], first['build']),
                         ('linux-appimage', 'beta', 5023))
        self.assertEqual(second['build'], 5024)
        notes = json.loads(self.store.objects[
            publisher.release_notes.key(source, 'linux-appimage', 'beta')])
        self.assertEqual((notes['build'], notes['previous_build'], notes['previous_source']),
                         (5024, 5023, SOURCE))

    def test_notes_inventory_failure_prevents_all_writes_and_feed_changes(self):
        with patch.object(publisher.release_notes, 'merged_items', side_effect=RuntimeError('inventory')):
            with self.assertRaisesRegex(RuntimeError, 'inventory'):
                self.publish()
        self.assertEqual(self.store.writes, [])

    def test_notes_write_failure_keeps_feed_and_retry_keeps_predecessor(self):
        previous = {'build': 5022, 'source': 'b' * 40,
                    'platform': 'linux-appimage', 'channel': 'beta'}
        current = json.dumps(previous).encode()
        self.store.objects['okilum/linux/beta/latest.json'] = current
        path = publisher.release_notes.key(SOURCE, 'linux-appimage', 'beta')
        self.store.fail = path
        with self.assertRaises(RuntimeError):
            self.publish()
        self.assertEqual(self.store.objects['okilum/linux/beta/latest.json'], current)
        self.store.fail = None
        self.publish()
        self.assertEqual(json.loads(self.store.objects[path])['previous_build'], 5022)

    def test_partial_upload_recovers_and_immutable_conflict_has_no_mutation(self):
        self.store.fail = 'okilum/linux/builds/5023/release.json'
        with self.assertRaises(RuntimeError):
            self.publish()
        self.assertFalse(any('/beta/' in key for key in self.store.objects))
        self.store.fail = None
        self.publish()
        self.store.objects['okilum/linux/builds/5023/' + artifact.IMAGE] += b'conflict'
        before = len(self.store.writes)
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual(len(self.store.writes), before)

    def test_signature_tamper_blocks_retry_and_newer_beta_is_never_replaced(self):
        self.publish()
        signature = 'okilum/linux/builds/5023/' + artifact.IMAGE + '.sig'
        self.store.objects[signature] = b'foreign signature'
        before = len(self.store.writes)
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual(len(self.store.writes), before)
        self.key.sign(self.root / artifact.IMAGE)
        self.store.objects[signature] = (self.root / (artifact.IMAGE + '.sig')).read_bytes()
        self.store.objects['okilum/linux/beta/latest.json'] = b'{"build":5024}'
        self.store.writes.clear()
        self.publish()
        self.assertFalse(any('/beta/' in key for key, _ in self.store.writes))

    def test_immutable_readback_failure_cannot_expose_beta(self):
        original_call = self.store.call
        def damaged_readback(method, key):
            value = original_call(method, key)
            if key.endswith('/provenance.json') and value is not None:
                return b'transfer corruption'
            return value
        with patch.object(self.store, 'call', side_effect=damaged_readback):
            with self.assertRaisesRegex(ValueError, 'readback'):
                self.publish()
        self.assertFalse(any('/beta/' in key for key in self.store.objects))

    def test_moving_failure_keeps_old_descriptor_and_retry_completes(self):
        self.store.objects['okilum/linux/beta/latest.json'] = json.dumps({'build':5022, 'source':'b'*40, 'platform':'linux-appimage', 'channel':'beta'}).encode()
        self.store.fail = 'okilum/linux/beta/latest.json'
        with self.assertRaises(RuntimeError):
            self.publish()
        self.assertEqual(self.store.objects['okilum/linux/beta/latest.json'], json.dumps({'build':5022, 'source':'b'*40, 'platform':'linux-appimage', 'channel':'beta'}).encode())
        self.store.fail = None
        self.publish()
        self.assertEqual(json.loads(self.store.objects['okilum/linux/beta/latest.json'])['build'], 5023)

    def test_wrong_source_version_or_bytes_and_signer_fail_before_mutation(self):
        for field, value in [('source_sha', 'b' * 40), ('build_version', '9999'),
                             ('release_version', '0.1.9999'), ('signed', True), ('workflow_run', '18'),
                             ('sha256', '0' * 64), ('workflow_attempt', '2'), ('acceptance_harness', True)]:
            (self.directory / 'provenance.json').write_text(json.dumps({**self.provenance, field: value}))
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.publish()
            self.assertEqual(self.store.writes, [])
        (self.directory / 'provenance.json').write_bytes(self.original)
        self.key.fingerprint = 'B' * 40
        with self.assertRaises(ValueError):
            self.publish()
        self.assertEqual(self.store.writes, [])


if __name__ == '__main__':
    unittest.main()
