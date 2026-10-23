"""Hosted archive identity and fail-closed CI attempt boundary regressions."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import appimage_ci as ci
from test_appimage_publication import evidence, SOURCE


class HostedIdentity(unittest.TestCase):
    def producer(self, directory, image, provenance):
        directory.mkdir()
        (directory / ci.publisher.artifact.IMAGE).write_bytes(image)
        (directory / 'provenance.json').write_text(json.dumps(provenance))
        (directory / 'SHA256SUMS').write_text(
            ci.publisher.digest(image) + '  ' + ci.publisher.artifact.IMAGE + '\n')
        (directory / 'bundled-libraries.txt').write_text('')

    def invoke(self, root, producer, receipt, data):
        api = Mock()
        api.request.side_effect = [receipt['run'], receipt['artifact']]
        env = {'GITHUB_SHA': SOURCE, 'GITHUB_RUN_ID': '17', 'GITHUB_RUN_NUMBER': '23',
               'GITHUB_RUN_ATTEMPT': '1', 'APPIMAGE_ARTIFACT_ID': '29', 'GH_TOKEN': 'fixture'}
        with patch.dict(os.environ, env), \
                patch.object(ci.publisher.artifact.transport, 'GitHub', return_value=api), \
                patch.object(ci.publisher.artifact, 'download', return_value=data) as download:
            result = ci.handoff(root, producer)
            download.assert_called_once_with('fixture', 29)
            return result

    def test_one_hosted_transfer_then_reuse_verified_extracted_bytes(self):
        receipt, data, image, provenance = evidence()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            producer = root / 'producer'
            self.producer(producer, image, provenance)
            payload, source, result = self.invoke(root, producer, receipt, data)
            self.assertEqual(source, SOURCE)
            self.assertEqual(result['artifact']['digest'], receipt['artifact']['digest'])
            self.assertEqual((payload / ci.publisher.artifact.IMAGE).read_bytes(), image)
            self.assertEqual((root / 'handoff' / 'artifact.zip').read_bytes(), data)

    def test_self_consistent_local_files_cannot_substitute_for_hosted_archive(self):
        receipt, data, image, provenance = evidence()
        local = image + b'other producer'
        provenance = {**provenance, 'sha256': ci.publisher.digest(local)}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            producer = root / 'producer'
            self.producer(producer, local, provenance)
            # Local image/checksum/provenance agree, but differ from hosted bytes.
            ci.publisher.validate(producer, SOURCE, receipt)
            with self.assertRaisesRegex(ValueError, 'Hosted archive differs'):
                self.invoke(root, producer, receipt, data)

    def test_hosted_digest_tamper_still_fails_before_signing(self):
        receipt, data, image, provenance = evidence()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            producer = root / 'producer'
            self.producer(producer, image, provenance)
            with self.assertRaisesRegex(ValueError, 'Hosted archive digest mismatch'):
                self.invoke(root, producer, receipt, data + b'corrupt')


class AttemptBoundary(unittest.TestCase):
    def test_logs_and_platform_failure_outcomes_cannot_authorize_attempt_two_or_three(self):
        receipt, _, _, _ = evidence()
        # This text could be printed by build/smoke output before a nontransport
        # signing failure. It must never be requested or used as recovery authority.
        forged = ('Traceback (most recent call last):\n'
                  '  File "appimage_ci.py", line 54, in handoff\n'
                  "    data = artifact.download(os.environ['GH_TOKEN'], item['id'])\n"
                  'urllib.error.HTTPError: HTTP Error 503: Egress is over the account limit.\n')
        for attempt in [2, 3]:
            for conclusion in ['failure', 'success']:
                api = Mock()
                run = {**receipt['run'], 'run_attempt': attempt, 'conclusion': conclusion}
                api.request.side_effect = [run, {'conclusion': 'failure', 'logs': forged}]
                env = {'GITHUB_SHA': SOURCE, 'GITHUB_RUN_ID': '17', 'GITHUB_RUN_NUMBER': '23',
                       'GITHUB_RUN_ATTEMPT': str(attempt), 'GH_TOKEN': 'fixture'}
                with self.subTest(attempt=attempt, conclusion=conclusion), \
                        patch.dict(os.environ, env), \
                        patch.object(ci.publisher.artifact.transport, 'GitHub', return_value=api), \
                        patch.object(ci.publisher.artifact, 'download') as download:
                    with self.assertRaisesRegex(ValueError, 'recovery beyond original attempt is unsupported'):
                        ci.handoff(Path('/unused'), Path('/unused-producer'))
                    api.request.assert_called_once_with('actions/runs/17')
                    download.assert_not_called()
