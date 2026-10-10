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
            api.call.side_effect = [{'status': state, 'commit_sha': sha, 'trigger_event': 'push'}, {'commit': {'id': 'latest'}}]
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
            archive.writestr('okilum.pkg.tar.zst', b'package')
        run = {'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'push',
               'status': 'success', 'commit_sha': 'source', 'index_in_repo': 42}
        for final_head, expected in [('source', True), ('newer', True)]:
            api = Mock()
            api.call.side_effect = [run, {'commit': {'id': 'source'}},
                [{'id': 9, 'name': 'arch-publication', 'expired': False, 'run_id': 12}],
                data.getvalue(), {'commit': {'id': final_head}}]
            with patch.object(publication.subprocess, 'run') as launch, patch.object(publication, 'R2') as store:
                store.return_value.call.return_value = None
                publication.publish(api, 'linux', 12)
                self.assertEqual(launch.called, expected)
                if expected:
                    self.assertIn('5042', launch.call_args.args[0])
                    self.assertEqual(launch.call_args.kwargs['env']['GITHUB_RUN_NUMBER'], '42')

    def test_linux_finishes_after_new_merge_but_rejects_newer_published_beta(self):
        import publication
        from unittest.mock import Mock
        run = {'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'push',
               'status': 'success', 'commit_sha': 'previous-main', 'index_in_repo': 42}
        self.assertTrue(publication.eligible(run, 'linux', 'new-main'))
        for changes in [{'prettyref': 'feature'}, {'is_fork_pull_request': True},
                        {'trigger_event': 'pull_request'}, {'workflow_id': 'ci.yml'}]:
            with self.assertRaises(ValueError):
                publication.eligible({**run, **changes}, 'linux', 'new-main')
        for status in ['failure', 'cancelled', 'running']:
            self.assertFalse(publication.eligible({**run, 'status': status}, 'linux', 'new-main'))
        api = Mock()
        api.call.side_effect = [run, {'commit': {'id': 'new-main'}}]
        with patch.object(publication, 'R2') as store, patch.object(publication.subprocess, 'run') as launch:
            store.return_value.call.return_value = json.dumps({'build': 5043}).encode()
            publication.publish(api, 'linux', 12)
            launch.assert_not_called()
            self.assertEqual(api.call.call_count, 2)

    def test_hourly_and_manual_snapshots_publish_after_main_advances(self):
        import publication
        import io
        import zipfile
        from unittest.mock import Mock
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as archive:
            archive.writestr('payload.zip', b'archive')
        for platform in ['macos', 'windows']:
            for event in ['schedule', 'workflow_dispatch']:
                run = {'workflow_id': publication.WORKFLOWS[platform], 'prettyref': 'main',
                       'is_fork_pull_request': False, 'trigger_event': event,
                       'status': 'success', 'commit_sha': 'selected-main', 'index_in_repo': 42}
                names = list(publication.ARTIFACTS[platform])
                artifacts = [{'id': i, 'name': name, 'expired': False, 'run_id': 12}
                             for i, name in enumerate(names)]
                api = Mock()
                api.call.side_effect = [run, {'commit': {'id': 'new-main'}}, artifacts,
                                       *[data.getvalue() for _ in names], {'commit': {'id': 'newer-main'}}]
                with patch.object(publication, 'R2') as store, patch.object(publication.subprocess, 'run') as launch:
                    store.return_value.call.return_value = None
                    publication.publish(api, platform, 12)
                    launch.assert_called_once()
                    self.assertEqual(launch.call_args.kwargs['env']['GITHUB_SHA'], 'selected-main')
                    self.assertEqual(launch.call_args.kwargs['env']['GITHUB_RUN_NUMBER'], '42')

    def test_each_feed_prevents_an_older_snapshot_from_publishing(self):
        import publication
        from unittest.mock import Mock
        feeds = {'linux': b'{"build": 5043}',
                 'windows': b'{"Assets": [{"Version": "0.1.5043"}]}',
                 'macos': b'<rss xmlns:s="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><item><s:version>5043</s:version></item></channel></rss>'}
        for platform, feed in feeds.items():
            run = {'workflow_id': publication.WORKFLOWS[platform], 'prettyref': 'main',
                   'is_fork_pull_request': False, 'trigger_event': 'schedule',
                   'status': 'success', 'commit_sha': 'old-main', 'index_in_repo': 42}
            api = Mock()
            api.call.side_effect = [run, {'commit': {'id': 'new-main'}}]
            with patch.object(publication, 'R2') as store, patch.object(publication.subprocess, 'run') as launch:
                store.return_value.call.return_value = feed
                self.assertEqual(publication.published_build(store(), platform), 5043)
                publication.publish(api, platform, 12)
                launch.assert_not_called()
                self.assertEqual(api.call.call_count, 2)


class ExplicitSourcePublication(unittest.TestCase):
    """#1040: a manual run that built an older main commit publishes to the archive only."""

    def publish(self, marker):
        import publication
        import io
        import zipfile
        from unittest.mock import Mock
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as archive:
            archive.writestr('okilum-0.1.5042-1-x86_64.pkg.tar.zst', b'package')
            if marker is not None:
                archive.writestr(publication.SOURCE_FILE, marker + '\n')
        run = {'workflow_id': 'linux-release.yml', 'prettyref': 'main',
               'is_fork_pull_request': False, 'trigger_event': 'workflow_dispatch',
               'status': 'success', 'commit_sha': 'c' * 40, 'index_in_repo': 42}
        api = Mock()
        api.call.side_effect = [run, {'commit': {'id': 'c' * 40}},
                                [{'id': 9, 'name': 'arch-publication', 'expired': False, 'run_id': 12}],
                                data.getvalue(), {'commit': {'id': 'c' * 40}}]
        launches = []

        def run_command(args, **kwargs):
            if args[:2] == ['git', 'merge-base']:
                return subprocess.CompletedProcess(args, 0)
            launches.append(args)
            return subprocess.CompletedProcess(args, 0)
        with patch.object(publication, 'R2') as store, \
                patch.object(publication.subprocess, 'run', side_effect=run_command):
            # The beta feed already holds a newer build than this run.
            store.return_value.call.return_value = json.dumps({'build': 9999}).encode()
            publication.publish(api, 'linux', 12)
        return launches

    def test_an_older_recorded_source_is_published_to_the_archive(self):
        launches = self.publish('d' * 40)
        self.assertEqual(len(launches), 1)
        command = launches[0]
        self.assertIn('--archive-only', command)
        self.assertEqual(command[command.index('--source') + 1], 'd' * 40)

    def test_without_a_marker_the_newer_feed_still_refuses_it(self):
        # Control: the same run without a recorded older source is an ordinary snapshot.
        self.assertEqual(self.publish(None), [])
        self.assertEqual(self.publish('c' * 40), [])

    def test_a_malformed_marker_is_refused(self):
        with self.assertRaisesRegex(ValueError, 'Invalid source'):
            self.publish('not-a-sha')


class MacOSArtifact(unittest.TestCase):
    def test_signed_archive_moves_between_hosts_and_preserves_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = root / 'notarized.zip'
            archive.write_bytes(b'original signed archive')
            dmg = root / 'Okilum-0.1.7000.dmg'
            dmg.write_bytes(b'original signed disk image')
            stage = root / 'stage'
            env = {'ARCHIVE': str(archive), 'DMG': str(dmg), 'BUILD': '7000', 'DISPLAY_VERSION': '0.1.7000',
                   'SOURCE_SHA': 'a' * 40, 'SOURCE_TREE': 'b' * 40, 'SIGNATURE': 'signature',
                   'GITHUB_SHA': 'a' * 40, 'GITHUB_RUN_NUMBER': '2000'}
            with patch.dict(os.environ, env), patch('sys.argv', ['stage', 'stage', str(stage)]):
                macos.main()
            archive.unlink()
            dmg.unlink()
            with patch.dict(os.environ, env), patch('sys.argv', ['publish', 'publish', str(stage)]), \
                 patch.object(macos, 'publish') as publish:
                macos.main()
                args = publish.call_args.args[0]
                self.assertEqual(Path(args.archive).read_bytes(), b'original signed archive')
                self.assertEqual(Path(args.dmg).read_bytes(), b'original signed disk image')
                self.assertEqual((args.build, args.source, args.signature), (7000, 'a' * 40, 'signature'))
                for key, value in [('SOURCE_SHA', 'c' * 40), ('BUILD', '7001'), ('archive', '../escape'),
                                   ('dmg', '../escape')]:
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
