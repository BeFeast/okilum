"""R2 authority/order controls: immutable ZIP fixtures and byte-bound fake signatures."""
import argparse
import copy
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

# The shared Arch fixture installs its CI fixture path, including when the
# workflow invokes unittest discovery without a repository-root PYTHONPATH.
from test_release_recovery import package_bytes, recovery
from test_linux_artifact_reuse import accepted_store, linux
from test_linux_binary import SHA, CONFIG, binary_bytes, package as payload
from test_prebuilt import Store, publication, publish

ROOT = Path(__file__).resolve().parents[2]


class PublicationAuthority(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.hosted = accepted_store(self.root)
        self.prebuilt = self.root / 'prebuilt'
        self.receipt = linux.download(self.hosted, linux.discover(self.hosted, SHA, CONFIG)[0],
                                      SHA, CONFIG, self.prebuilt)
        self.name = publish.package_name(14108)
        self.prefix = 'okilum/arch/builds/14108'
        (self.root / 'arch').mkdir()
        self.path = self.root / 'arch' / self.name
        self.data = package_bytes(r=self.receipt)
        self.path.write_bytes(self.data)
        (self.root / 'arch/production-receipt.json').write_bytes(payload.encode(self.receipt))
        self.signature = b'exact original fixture signature'
        self.manifest = dict(build=14108, source=SHA, filename=self.name,
            sha256=payload.digest(self.data), production=self.receipt,
            binary_sha256=payload.digest(binary_bytes()))
        self.store = Store()
        self.original = {f'{self.prefix}/manifest.json': payload.encode(self.manifest),
            f'{self.prefix}/{self.name}': self.data,
            f'{self.prefix}/{self.name}.sig': self.signature}
        self.downloads = []
        fixture = self

        class AuthAPI:
            def __init__(self, token):
                self.token = token

            def request(self, path, method='GET', data=None):
                fixture.assertEqual(method, 'GET')
                return fixture.hosted.request(path, method, data)

            def download_artifact(self, artifact_id):
                allowed = self.token == 'fixture-existing-read-token'
                fixture.downloads.append(allowed)
                if not allowed:
                    raise linux.transport.Unavailable('Fixture ZIP authentication required (401)')
                return fixture.hosted.download_artifact(artifact_id)

        self.enterContext(patch.dict(os.environ, GH_TOKEN='fixture-existing-read-token', MIRROR_TOKEN=''))
        self.enterContext(patch.object(linux.transport, 'GitHub', AuthAPI))
        self.enterContext(patch.object(linux, 'archived', return_value=None))
        for module in (publication.archive_authority, publish.archive_authority, recovery):
            self.enterContext(patch.object(module.payload, 'receipt_authority', return_value=linux))
            self.enterContext(patch.object(module.consumer, 'public_read', side_effect=lambda key: self.store.objects.get(key)))
            self.enterContext(patch.object(module.consumer, 'verify_signature', side_effect=self.verify))
        self.enterContext(patch.object(recovery.payload, 'config', return_value=CONFIG))
        self.key = Mock()
        self.key.verify.side_effect = lambda path: self.verify(path, Path(str(path) + '.sig'))
        self.key.sign.side_effect = AssertionError('Retry must not sign replacement package')
        self.key.public.return_value = b'fixture public key'
        self.channel = self.enterContext(patch.object(publish, 'publish_channel'))
        self.enterContext(patch.object(publish, 'validate_package'))
        self.args = argparse.Namespace(command='publish', build=14108, source=SHA,
                                       package=str(self.path), archive_only=False)

    def verify(self, package, signature):
        if package.read_bytes() != self.data or signature.read_bytes() != self.signature:
            raise ValueError('Configured fixture signature mismatch')

    def downstream(self):
        with tempfile.TemporaryDirectory() as directory:
            publish.execute(self.args, self.store, self.key, Path(directory))

    def protected(self):
        """Real protected publish -> production_build -> real downstream execute."""
        run = dict(workflow_id='linux-release.yml', prettyref='main', is_fork_pull_request=False,
            trigger_event='schedule', status='success', commit_sha=SHA, index_in_repo=9999)
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, 'w') as archive:
            archive.writestr(self.name, self.path.read_bytes())
            archive.writestr('production-receipt.json', (self.root / 'arch/production-receipt.json').read_bytes())
            archive.writestr('release-source.txt', SHA + '\n')
        api = Mock()
        api.call.side_effect = [run, {'commit': {'id': SHA}},
            [dict(id=7, name='arch-publication', expired=False, run_id=12)], stream.getvalue(),
            {'commit': {'id': SHA}}]

        original_run = subprocess.run
        def child(command, **kwargs):
            if str(ROOT / 'scripts/arch/publish.py') not in command:
                return original_run(command, **kwargs)
            env, check = kwargs['env'], kwargs['check']
            self.assertTrue(check)
            self.assertIn(str(ROOT / 'scripts/arch/publish.py'), command)
            self.assertEqual(command[command.index('--build') + 1], '14108')
            self.assertEqual(env['GH_TOKEN'], 'fixture-existing-read-token')
            args = argparse.Namespace(**vars(self.args))
            args.package = command[command.index('--package') + 1]
            with patch.dict(os.environ, env), tempfile.TemporaryDirectory() as directory:
                publish.execute(args, self.store, self.key, Path(directory))

        with patch.object(publication, 'R2', return_value=self.store), \
                patch.object(publication.prepare, 'complete'), \
                patch.object(publication.subprocess, 'run', side_effect=child) as launch:
            publication.publish(api, 'linux', 12)
        return [call for call in launch.call_args_list if str(ROOT / 'scripts/arch/publish.py') in call.args[0]]

    def test_fresh_packaging_and_protected_publication_require_existing_read_token(self):
        # Same fresh source/package/ZIP; only the read credential changes.
        self.assertFalse(recovery.reuse(SHA, self.prebuilt, self.root / 'out',
                         fetch=self.store.objects.get, verify=self.verify))
        self.assertEqual(publication.production_build(self.root, SHA), 14108)
        with patch.dict(os.environ, GH_TOKEN='', MIRROR_TOKEN='fixture-existing-read-token'):
            self.assertFalse(recovery.reuse(SHA, self.prebuilt, self.root / 'mirror-out',
                             fetch=self.store.objects.get, verify=self.verify))
            self.assertEqual(publication.production_build(self.root, SHA), 14108)
        with patch.dict(os.environ, GH_TOKEN='', MIRROR_TOKEN='', FORGEJO_TOKEN='fixture-forgejo-only'):
            for operation in (lambda: recovery.reuse(SHA, self.prebuilt, self.root / 'out',
                                fetch=self.store.objects.get, verify=self.verify),
                              lambda: publication.production_build(self.root, SHA), self.downstream):
                with self.assertRaisesRegex(linux.transport.Unavailable, '401'):
                    operation()
        self.assertEqual(self.downloads, [True, True, True, True, False, False, False])
        self.key.sign.assert_not_called()
        self.channel.assert_not_called()
        self.assertEqual(self.store.objects, {})

    def test_actual_fresh_protected_path_and_child_authenticate_with_existing_token(self):
        # Fake signature only: this exercises initial publication, including the
        # real downstream validator and inherited child environment.
        self.key.sign.side_effect = lambda path: Path(str(path) + '.sig').write_bytes(self.signature)
        self.assertEqual(len(self.protected()), 1)
        self.assertEqual(self.downloads, [True, True])
        self.key.sign.assert_called_once()
        self.store.objects.clear()
        before = self.channel.call_count
        with patch.dict(os.environ, GH_TOKEN='', MIRROR_TOKEN='', FORGEJO_TOKEN='fixture-forgejo-only'):
            with self.assertRaisesRegex(linux.transport.Unavailable, '401'):
                self.protected()
        self.assertEqual(self.channel.call_count, before)
        self.assertEqual(self.store.objects, {})
        self.assertEqual(self.downloads, [True, True, False])

    def test_actual_protected_retry_uses_signed_build_after_expiry_without_descriptor(self):
        self.store.objects.update(self.original)
        # Unexpired identical input is the original-path positive control.
        self.assertEqual(len(self.protected()), 1)
        self.assertEqual(self.downloads, [])
        self.hosted.artifacts[1][0]['expired'] = True
        with self.assertRaisesRegex(ValueError, 'Production artifact expired'):
            publication.native_payload.validate_receipt(self.receipt, SHA, CONFIG)
        # Remove the descriptor written by the positive fake publication.
        self.store.objects = dict(self.original)
        self.assertNotIn(f'okilum/releases/{SHA}/linux.json', self.store.objects)
        self.assertEqual(len(self.protected()), 1)
        self.key.sign.assert_not_called()
        self.assertEqual(self.downloads, [])
        for name, data in self.original.items():
            self.assertEqual(self.store.objects[name], data)
        self.assertEqual(self.channel.call_args.args[5:7], (self.data, self.signature))

    def test_absent_signed_archive_cannot_authorize_expired_unsigned_receipt(self):
        self.hosted.artifacts[1][0]['expired'] = True
        for operation in (self.protected, self.downstream):
            with self.assertRaisesRegex(ValueError, 'Production artifact expired'):
                operation()
        self.assertEqual(self.store.objects, {})
        self.channel.assert_not_called()
        self.key.sign.assert_not_called()

    def test_both_publication_layers_reject_partial_invalid_and_changed_archives(self):
        self.hosted.artifacts[1][0]['expired'] = True
        for case in ('missing_manifest', 'missing_package', 'missing_signature', 'signature',
                     'tamper', 'source', 'config', 'artifact_identity', 'binary', 'selected_package', 'receipt'):
            with self.subTest(case=case):
                self.store.objects = copy.deepcopy(self.original)
                self.path.write_bytes(self.data)
                (self.root / 'arch/production-receipt.json').write_bytes(payload.encode(self.receipt))
                m = copy.deepcopy(self.manifest)
                if case.startswith('missing_'):
                    suffix = {'missing_manifest': 'manifest.json', 'missing_package': self.name,
                              'missing_signature': self.name + '.sig'}[case]
                    del self.store.objects[f'{self.prefix}/{suffix}']
                elif case == 'signature':
                    self.store.objects[f'{self.prefix}/{self.name}.sig'] = b'wrong signer'
                elif case == 'tamper':
                    altered = self.data + b'tampered'
                    self.store.objects[f'{self.prefix}/{self.name}'] = altered
                    m['sha256'] = payload.digest(altered)
                elif case == 'source':
                    m['source'] = 'b' * 40
                elif case in ('config', 'artifact_identity'):
                    m['production']['manifest']['config'] = 'b' * 64
                elif case == 'binary':
                    m['binary_sha256'] = 'b' * 64
                elif case == 'selected_package':
                    self.path.write_bytes(package_bytes(builddate=2, r=self.receipt))
                elif case == 'receipt':
                    bad = copy.deepcopy(self.receipt)
                    bad['artifact_id'] += 1
                    (self.root / 'arch/production-receipt.json').write_bytes(payload.encode(bad))
                if case == 'artifact_identity':
                    m['production'] = copy.deepcopy(self.receipt)
                    m['production']['artifact_id'] += 1
                if case != 'missing_manifest':
                    self.store.objects[f'{self.prefix}/manifest.json'] = payload.encode(m)
                before = copy.deepcopy(self.store.objects)
                with self.assertRaises(ValueError):
                    self.protected()
                # The standalone downstream validator must fail independently.
                if case != 'receipt':  # Downstream has only the embedded receipt.
                    with self.assertRaises(ValueError):
                        self.downstream()
                self.assertEqual(self.store.objects, before)
                self.key.sign.assert_not_called()
                self.channel.assert_not_called()


class WorkflowReadCredentials(unittest.TestCase):
    def test_every_hosted_authority_stage_has_appropriate_existing_read_credential(self):
        hosted = (ROOT / '.github/workflows/forgejo-arch.yml').read_text()
        forgejo = (ROOT / '.forgejo/workflows/linux-release.yml').read_text()
        protected = (ROOT / '.forgejo/workflows/release-publish.yml').read_text()
        for workflow in (hosted, forgejo):
            steps = re.split(r'\n\s+- name: ', workflow)
            for name in ('Resolve exact required-CI production bytes', 'Build package as an unprivileged user'):
                step = next(s for s in steps if s.startswith(name))
                if workflow == hosted:
                    self.assertIn('GH_TOKEN: ${{ github.token }}', step)
                else:
                    self.assertIn("MIRROR_TOKEN: ${{ (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && secrets.OKILUM_GITHUB_MIRROR || '' }}", step)
            build = next(s for s in steps if s.startswith('Build package as an unprivileged user'))
            self.assertIn('runuser -u builder -- env OKILUM_SETTINGS_UI_HARNESS=0', build)
            self.assertNotIn('env -i', build)
            self.assertNotIn('--login', build)
        step = protected.split('name: Publish only a complete current-main build', 1)[1].split('- name:', 1)[0]
        self.assertIn("MIRROR_TOKEN: ${{ inputs.platform == 'linux' && secrets.OKILUM_GITHUB_MIRROR || '' }}", step)
        self.assertIn("if: github.ref == 'refs/heads/main'", protected)
        fork = forgejo.split('name: Produce isolated unsigned fork bytes', 1)[1].split('- name:', 1)[0]
        self.assertNotIn('secrets.', fork)


if __name__ == '__main__':
    unittest.main()
