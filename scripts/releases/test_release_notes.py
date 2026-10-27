"""Offline range, channel, filtering and sidecar contract regressions."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

import catalog
import release_notes as notes

SOURCE = 'a' * 40
OLD = 'b' * 40


class Store:
    def __init__(self, data=None):
        self.data = data or {}
        self.writes = []

    def call(self, method, key):
        return self.data.get(key)

    def put(self, key, data, *args):
        self.data[key] = data
        self.writes.append(key)


def pr(number, title, **kwargs):
    return {'number': number, 'title': title, 'body': '', 'labels': [], **kwargs}


def feed(*entries):
    root = ET.Element('rss'); channel = ET.SubElement(root, 'channel')
    for build, source, name in entries:
        item = ET.SubElement(channel, 'item')
        for tag, text in [(notes.SPARKLE + 'version', str(build)),
                          (notes.SPARKLE + 'channel', name), (notes.OKILUM + 'source', source)]:
            ET.SubElement(item, tag).text = text
    return ET.tostring(root)


class Filtering(unittest.TestCase):
    def test_override_and_conventional_titles_and_labels(self):
        self.assertEqual(notes.pr_item(pr(1, 'feat(reader)!: New outline')),
                         {'pr': 1, 'kind': 'feature', 'text': 'New outline'})
        self.assertEqual(notes.pr_item(pr(2, 'fix: Keep notes', body='User-visible: Safe save\nMore detail')),
                         {'pr': 2, 'kind': 'fix', 'text': 'Safe save'})
        self.assertEqual(notes.pr_item(pr(3, 'Reader outline', labels=[{'name': 'enhancement'}]))['kind'], 'feature')
        self.assertEqual(notes.pr_item(pr(4, 'Keep notes', labels=[{'name': 'bug'}]))['kind'], 'fix')
        self.assertEqual(notes.pr_item(pr(5, 'refactor: Writer', body='User-visible: Faster save'))['text'], 'Faster save')

    def test_internal_changes_hidden_unless_explicit(self):
        for prefix in ['ci', 'test', 'chore', 'docs', 'refactor']:
            with self.subTest(prefix=prefix):
                self.assertIsNone(notes.pr_item(pr(1, f'{prefix}(reader): Internal work')))
                self.assertEqual(notes.pr_item(pr(1, f'{prefix}: Internal work', body='User-visible: Faster open'))['text'], 'Faster open')
        self.assertIsNone(notes.pr_item(pr(1, 'Internal work', labels=[{'name': 'ci'}])))
        self.assertIsNone(notes.pr_item(pr(1, 'docs: Internal', body='User-visible:  \n\nNot a summary')))
        self.assertIsNone(notes.pr_item(pr(1, 'docs: Internal', body='<!--\nUser-visible: example\n-->')))

    def test_html_in_authored_text_is_only_text(self):
        payload = {'items': [notes.pr_item(pr(42, 'feat: Outline <script> & code'))]}
        html = notes.description(payload)
        self.assertIn('Outline &lt;script&gt; &amp; code', html)
        self.assertIn('/pulls/42', html)
        self.assertNotIn('<script>', html)


class PreviousChannel(unittest.TestCase):
    def test_macos_uses_beta_predecessor_not_higher_stable_or_other_platform(self):
        store = Store({'okilum/appcast.xml': feed((101, OLD, 'beta'), (200, SOURCE, 'stable')),
                       'okilum/arch/beta/x86_64/latest.json': b'{"build":500}'})
        self.assertEqual(notes.predecessor(store, 'macos'), {'build': 101, 'source': OLD})
        self.assertIsNone(notes.predecessor(Store({'okilum/appcast.xml': feed((200, SOURCE, 'stable'))}), 'macos'))

    def test_linux_and_windows_capture_their_own_actual_published_head(self):
        store = Store({'okilum/arch/beta/x86_64/latest.json': json.dumps({'build': 102, 'source': OLD}).encode(),
                       'okilum/windows/beta/releases.beta.json': b'{"Assets":[{"Version":"0.1.103"},{"Version":"0.1.101"}]}',
                       'okilum/windows/builds/103/release.json': json.dumps({'build': 103, 'source': SOURCE}).encode()})
        self.assertEqual(notes.predecessor(store, 'linux'), {'build': 102, 'source': OLD})
        self.assertEqual(notes.predecessor(store, 'windows'), {'build': 103, 'source': SOURCE})
        with self.assertRaises(ValueError):
            notes.predecessor(store, 'macos', 'unknown')
        for platform in ['macos', 'linux', 'windows']:
            self.assertIsNone(notes.predecessor(Store(), platform))

    @patch.object(notes, 'merged_items', return_value=[{'pr': 42, 'kind': 'feature', 'text': 'Outline'}])
    def test_exact_body_and_separate_platform_keys_leave_descriptor_unchanged(self, items):
        store = Store({'okilum/appcast.xml': feed((101, OLD, 'beta'))})
        descriptor = {'platform': 'macos', 'build': 104, 'source': SOURCE, 'assets': []}
        catalog.record(store, 'macos', 104, SOURCE, [])
        before = store.data[f'okilum/releases/{SOURCE}/macos.json']
        payload = notes.prepare(store, Mock(), 'macos', 104, SOURCE)
        self.assertEqual(payload, {'build': 104, 'source': SOURCE, 'previous_build': 101,
                                  'previous_source': OLD, 'platform': 'macos', 'channel': 'beta',
                                  'items': [{'pr': 42, 'kind': 'feature', 'text': 'Outline'}]})
        notes.record(store, payload)
        items.assert_called_once_with(unittest.mock.ANY, OLD, SOURCE)
        self.assertEqual(json.loads(before), descriptor)
        self.assertEqual(store.data[f'okilum/releases/{SOURCE}/macos.json'], before)
        self.assertEqual(store.writes[-1], f'okilum/releases/{SOURCE}/macos/beta/notes.json')
        self.assertNotEqual(notes.key(SOURCE, 'macos', 'beta'), notes.key(SOURCE, 'windows', 'beta'))
        self.assertNotIn(f'okilum/releases/{SOURCE}/notes.json', store.data)


class MergedRange(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(); self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git('init', '-b', 'main')
        # Disposable repositories must have no background writers during strict cleanup.
        self.git('config', 'gc.auto', '0')
        self.git('config', 'maintenance.auto', 'false')
        self.git('config', 'gc.autoDetach', 'false')
        self.git('config', 'user.name', 'Offline test'); self.git('config', 'user.email', 'test@example.invalid')
        self.old = self.commit('Previous published main')
        self.git('branch', 'feature')
        self.previous = self.commit('Previous build')
        self.git('checkout', 'feature'); self.branch = self.commit('Old branch content')
        self.git('checkout', 'main'); self.git('merge', '--no-ff', 'feature', '-m', 'Merged feature PR')
        self.source = self.git('rev-parse', 'HEAD')
        self.future = self.commit('Future merge outside release')
        self.client = Mock()
        self.client.call.return_value = [
            pr(1, 'feat: Old', merged=True, merge_commit_sha=self.previous),
            pr(2, 'feat: Outline', merged=True, merge_commit_sha=self.source),
            pr(3, 'fix: Future', merged=True, merge_commit_sha=self.future),
            pr(4, 'fix: Not merged', merged=False, merge_commit_sha=self.source),
            pr(5, 'fix: Branch commit is not its merge', merged=True, merge_commit_sha=self.branch)]

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, text=True, stderr=subprocess.DEVNULL).strip()

    def commit(self, message):
        self.git('commit', '--allow-empty', '-m', message)
        return self.git('rev-parse', 'HEAD')

    def select(self, previous, source):
        execute = subprocess.run
        with patch.object(notes.subprocess, 'run', side_effect=lambda *a, **k: execute(*a, cwd=self.root, **k)):
            return notes.merged_items(self.client, previous, source)

    def test_disposable_repository_disables_automatic_maintenance(self):
        for key, expected in [('gc.auto', '0'), ('maintenance.auto', 'false'),
                              ('gc.autoDetach', 'false')]:
            with self.subTest(key=key):
                self.assertEqual(self.git('config', '--local', '--get', key), expected)

    def test_first_parent_range_uses_merge_commit_not_pr_dates_or_branch_commits(self):
        self.assertEqual(self.select(self.previous, self.source),
                         [{'pr': 2, 'kind': 'feature', 'text': 'Outline'}])

    def test_empty_range_and_first_publication(self):
        self.assertEqual(self.select(self.source, self.source), [])
        self.client.call.assert_not_called()
        self.assertIn('Maintenance and stability fixes', notes.description({'items': []}))
        self.assertEqual([x['pr'] for x in self.select(None, self.source)], [2, 1])

    def test_stable_range_accumulates_several_beta_builds(self):
        # Stable skipped both intermediate beta builds; its own baseline wins.
        store = Store({'okilum/appcast.xml': feed((700, self.previous, 'stable'),
                       (750, self.source, 'beta'), (760, self.future, 'beta'))})
        execute = subprocess.run
        with patch.object(notes.subprocess, 'run', side_effect=lambda *a, **k: execute(*a, cwd=self.root, **k)):
            payload = notes.prepare_stable(store, self.client, 'macos', 760, self.future)
        self.assertEqual(payload['previous_source'], self.previous)
        self.assertEqual(payload['previous_build'], 700)
        self.assertEqual(payload['channel'], 'stable')
        self.assertEqual([item['pr'] for item in payload['items']], [3, 2])
        notes.record(store, payload)
        self.assertEqual(store.writes, [notes.key(self.future, 'macos', 'stable')])

    def test_stable_empty_range_uses_maintenance(self):
        store = Store({'okilum/appcast.xml': feed((700, self.source, 'stable'))})
        execute = subprocess.run
        with patch.object(notes.subprocess, 'run', side_effect=lambda *a, **k: execute(*a, cwd=self.root, **k)):
            payload = notes.prepare_stable(store, self.client, 'macos', 701, self.source)
        self.assertEqual(payload['items'], [])
        self.assertEqual(payload['previous_build'], 700)
        self.assertIn(notes.MAINTENANCE, notes.description(payload))

    def test_archive_only_older_source_keeps_real_beta_predecessor_and_empty_range(self):
        # Historical archive builds do not move beta. Their source is already
        # contained in the newer published beta, so they introduce no new PRs.
        store = Store({'okilum/arch/beta/x86_64/latest.json':
                       json.dumps({'build': 9999, 'source': self.future}).encode()})
        execute = subprocess.run
        with patch.object(notes.subprocess, 'run', side_effect=lambda *a, **k: execute(*a, cwd=self.root, **k)):
            payload = notes.prepare(store, self.client, 'linux', 5042, self.source)
        self.assertEqual(payload['previous_source'], self.future)
        self.assertEqual(payload['previous_build'], 9999)
        self.assertEqual(payload['items'], [])
        self.assertIn(notes.MAINTENANCE, notes.description(payload))
        self.client.call.assert_not_called()
        self.assertEqual(store.writes, [])

    @patch.object(notes.subprocess, 'run')
    def test_paginated_pr_inventory_is_complete_and_deduplicated(self, run):
        run.return_value.stdout = self.source + '\n'
        selected = pr(42, 'fix: Keep notes', merged=True, merge_commit_sha=self.source)
        self.client.call.side_effect = [[pr(i, 'docs: Internal') for i in range(50)], [selected, selected]]
        self.assertEqual(notes.merged_items(self.client, self.previous, self.source),
                         [{'pr': 42, 'kind': 'fix', 'text': 'Keep notes'}])
        self.assertIn('page=2', self.client.call.call_args.args[1])


class PublicationSidecar(unittest.TestCase):
    def test_capture_before_publish_and_only_add_sidecar_without_descriptor_changes(self):
        import io
        import zipfile
        import publication
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, 'w') as z:
            z.writestr('payload.zip', b'original payload')
            z.writestr('okilum.pkg.tar.zst', b'original package')
            z.writestr('release-source.txt', SOURCE)
        expected_items = [{'pr': 42, 'kind': 'feature', 'text': 'Outline'}]
        for platform in ['macos', 'windows', 'linux']:
            with self.subTest(platform=platform):
                store = Store({'okilum/appcast.xml': feed((5000, OLD, 'beta')),
                    'okilum/arch/beta/x86_64/latest.json': json.dumps({'build': 5000, 'source': OLD}).encode(),
                    'okilum/windows/beta/releases.beta.json': b'{"Assets":[{"Version":"0.1.5000"}]}',
                    'okilum/windows/builds/5000/release.json': json.dumps({'build': 5000, 'source': OLD}).encode(),
                    f'okilum/releases/{SOURCE}/{platform}.json': b'unchanged descriptor bytes'})
                run = {'workflow_id': publication.WORKFLOWS[platform], 'prettyref': 'main',
                       'is_fork_pull_request': False, 'trigger_event': 'schedule',
                       'status': 'success', 'commit_sha': SOURCE, 'index_in_repo': 42}
                client = Mock()
                def api(method, path, **kwargs):
                    if path == '/actions/runs/12':
                        return run
                    if path == '/branches/main':
                        return {'commit': {'id': SOURCE}}
                    if path.endswith('/artifacts'):
                        return [{'id': i, 'name': name, 'run_id': 12, 'expired': False}
                                for i, name in enumerate(publication.ARTIFACTS[platform])]
                    if path.endswith('/zip'):
                        return archive.getvalue()
                    raise AssertionError('Unexpected API call: ' + path)
                client.call.side_effect = api
                # Missing range/API evidence fails visibly BEFORE the unchanged
                # publishers or any sidecar write, for all three platforms.
                with patch.object(publication, 'R2', return_value=store), \
                        patch.object(publication.subprocess, 'run') as launch, \
                        patch.object(notes, 'merged_items', side_effect=RuntimeError('incomplete notes evidence')):
                    with self.assertRaisesRegex(RuntimeError, 'incomplete notes evidence'):
                        publication.publish(client, platform, 12)
                    launch.assert_not_called()
                    self.assertEqual(store.writes, [])
                commands = []
                def publish(command, **kwargs):
                    env = kwargs['env']
                    commands.append(command)
                    self.assertEqual(env['GITHUB_SHA'], SOURCE)
                    self.assertEqual(env['GITHUB_RUN_NUMBER'], '42')
                    if platform == 'macos':
                        staged = json.loads(Path(env['OKILUM_RELEASE_NOTES']).read_text())
                        self.assertEqual(staged['previous_build'], 5000)
                        self.assertEqual(staged['previous_source'], OLD)
                    else:
                        self.assertNotIn('OKILUM_RELEASE_NOTES', env)
                    # Simulate changing a feed inside the unchanged publisher.
                    store.data['okilum/appcast.xml'] = feed((5042, SOURCE, 'beta'))
                    store.data['okilum/arch/beta/x86_64/latest.json'] = json.dumps({'build': 5042, 'source': SOURCE}).encode()
                    store.data['okilum/windows/beta/releases.beta.json'] = b'{"Assets":[{"Version":"0.1.5042"}]}'
                with patch.object(publication, 'R2', return_value=store), \
                        patch.object(publication.subprocess, 'run', side_effect=publish), \
                        patch.object(notes, 'merged_items', return_value=expected_items), \
                        patch.object(publication.prepare, 'complete'):
                    publication.publish(client, platform, 12)
                result = json.loads(store.data[notes.key(SOURCE, platform, 'beta')])
                self.assertEqual((result['previous_build'], result['previous_source']), (5000, OLD))
                self.assertEqual(result['items'], expected_items)
                self.assertEqual(store.data[f'okilum/releases/{SOURCE}/{platform}.json'], b'unchanged descriptor bytes')
                self.assertEqual(store.writes, [notes.key(SOURCE, platform, 'beta')])
                self.assertEqual(len(commands), 1)
                self.assertNotIn('--archive-only', commands[-1])
