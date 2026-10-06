"""Exercise stale-head suppression and artifact provenance before publication."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('macos_artifact', HERE / 'macos-artifact.py')
macos = importlib.util.module_from_spec(spec)
spec.loader.exec_module(macos)


class PublicationBoundary(unittest.TestCase):
    def test_only_successful_current_main_build_is_eligible(self):
        import publication
        run = {'workflow_id': 'windows-diagnostic.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'push',
               'status': 'success', 'commit_sha': 'latest'}
        self.assertTrue(publication.eligible(run, 'windows', 'latest'))
        self.assertTrue(publication.eligible({**run, 'trigger_event': 'schedule'}, 'windows', 'latest'))
        self.assertFalse(publication.eligible(run, 'windows', 'newer'))
        for state in ['running', 'cancelled', 'failure']:
            self.assertFalse(publication.eligible({**run, 'status': state}, 'windows', 'latest'))
        for changes in [{'prettyref': 'feature'}, {'trigger_event': 'pull_request'},
                        {'is_fork_pull_request': True}, {'workflow_id': 'ci.yml'}]:
            with self.assertRaises(ValueError):
                publication.eligible({**run, **changes}, 'windows', 'latest')

    def test_cancelled_or_superseded_run_never_downloads_or_publishes(self):
        import publication
        from unittest.mock import Mock
        for state, sha in [('cancelled', 'latest'), ('success', 'old')]:
            api = Mock()
            api.call.side_effect = [{'status': state, 'commit_sha': sha}, {'commit': {'id': 'latest'}}]
            with patch.object(publication.subprocess, 'run') as launch:
                publication.publish(api, 'windows', 12)
                launch.assert_not_called()
            self.assertEqual(api.call.call_count, 2)

    def test_publication_preserves_original_build_number_and_rechecks_main(self):
        import publication
        import io
        import zipfile
        from unittest.mock import Mock
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as archive:
            archive.writestr('tessera.pkg.tar.zst', b'package')
        run = {'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'push',
               'status': 'success', 'commit_sha': 'source', 'index_in_repo': 42}
        for final_head, expected in [('source', True), ('newer', False)]:
            api = Mock()
            api.call.side_effect = [run, {'commit': {'id': 'source'}},
                [{'id': 9, 'name': 'arch-publication', 'expired': False, 'run_id': 12}],
                data.getvalue(), {'commit': {'id': final_head}}]
            with patch.object(publication.subprocess, 'run') as launch:
                publication.publish(api, 'linux', 12)
                self.assertEqual(launch.called, expected)
                if expected:
                    self.assertIn('5042', launch.call_args.args[0])
                    self.assertEqual(launch.call_args.kwargs['env']['GITHUB_RUN_NUMBER'], '42')


class MacOSArtifact(unittest.TestCase):
    def test_signed_archive_moves_between_hosts_and_preserves_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / 'notarized.zip'
            archive.write_bytes(b'original signed archive')
            stage = root / 'stage'
            env = {'ARCHIVE': str(archive), 'BUILD': '7000', 'DISPLAY_VERSION': '0.1.7000',
                   'SOURCE_SHA': 'a' * 40, 'SOURCE_TREE': 'b' * 40, 'SIGNATURE': 'signature',
                   'GITHUB_SHA': 'a' * 40, 'GITHUB_RUN_NUMBER': '2000'}
            with patch.dict(os.environ, env), patch('sys.argv', ['stage', 'stage', str(stage)]):
                macos.main()
            archive.unlink()
            with patch.dict(os.environ, env), patch('sys.argv', ['publish', 'publish', str(stage)]), \
                 patch.object(macos, 'publish') as publish:
                macos.main()
                args = publish.call_args.args[0]
                self.assertEqual(Path(args.archive).read_bytes(), b'original signed archive')
                self.assertEqual((args.build, args.source, args.signature), (7000, 'a' * 40, 'signature'))
                for key, value in [('SOURCE_SHA', 'c' * 40), ('BUILD', '7001'), ('archive', '../escape')]:
                    metadata = json.loads((stage / 'release.json').read_text())
                    original = metadata.copy()
                    metadata[key] = value
                    (stage / 'release.json').write_text(json.dumps(metadata))
                    publish.reset_mock()
                    with self.assertRaises(ValueError):
                        macos.main()
                    publish.assert_not_called()
                    (stage / 'release.json').write_text(json.dumps(original))


if __name__ == '__main__':
    unittest.main()
