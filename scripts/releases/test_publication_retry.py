"""A failed publication of a completed build is retried, bounded, and never repeated after success (#1559)."""
from pathlib import Path
import json
import os
import tempfile
import types
import unittest
from unittest.mock import patch

import xml.etree.ElementTree as ET

import yaml

import publication
import release
import release_notes

ROOT = Path(__file__).resolve().parents[2]
DISPATCH = '/actions/workflows/release-publish.yml/dispatches'


class Feeds:
    """Public feed reader double: a macOS appcast whose beta head is `head`."""
    def __init__(self, head=None):
        self.head = head

    def call(self, method, key):
        if self.head is None:
            return None
        build, source = self.head
        return (f'<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" '
                f'xmlns:okilum="https://git.oklabs.uk/BeFeast/okilum/update-metadata"><channel>'
                f'<item><sparkle:version>{build}</sparkle:version><sparkle:channel>beta</sparkle:channel>'
                f'<okilum:source>{source}</okilum:source></item></channel></rss>').encode()


class FakeForgejo:
    def __init__(self, issues=(), fail_posts=0, index=None):
        self.calls = []
        self.issues = list(issues)
        self.fail_posts = fail_posts
        self.index = index

    def call(self, method, path, body=None):
        self.calls.append((method, path, body))
        if method == 'GET' and path.startswith('/actions/runs/'):
            if self.index is None:
                raise SystemExit(f'GET {path}: 404')
            return {'index_in_repo': self.index}
        if method == 'POST' and path == DISPATCH and self.fail_posts:
            self.fail_posts -= 1
            raise SystemExit(f'POST {path}: 502')
        if method == 'GET' and path.startswith('/issues?'):
            return self.issues
        return None

    def dispatches(self):
        return [body for method, path, body in self.calls if method == 'POST' and path == DISPATCH]


def release_publish_jobs():
    return yaml.load((ROOT / '.forgejo/workflows/release-publish.yml').read_text(),
                     Loader=yaml.BaseLoader)


def run_publication_chain(client, platform, run_id, outcomes):
    """Dry-run of release-publish: each dispatched run publishes, and on failure the retry job runs.

    `outcomes` is the publish job result of each successive release-publish run.
    Returns the publish results that ran and whether the chain ended red.
    """
    pending = [{'platform': platform, 'run_id': str(run_id)}]
    results = []
    ended_red = False
    while pending:
        inputs = pending.pop(0)
        result = outcomes[len(results)]
        results.append(result)
        # The workflow gate of the retry job, evaluated as Forgejo would.
        if result != 'failure':
            continue
        before = len(client.dispatches())
        try:
            publication.retry(client, inputs['platform'], int(inputs['run_id']),
                              int(inputs.get('attempt', '1')), sleep=lambda _: None, feeds=Feeds())
        except SystemExit:
            ended_red = True
        pending.extend(body['inputs'] for body in client.dispatches()[before:])
    return results, ended_red


class PublicationRetry(unittest.TestCase):
    def test_failed_publication_is_redispatched_for_the_same_build(self):
        client = FakeForgejo()
        delays = []
        publication.retry(client, 'macos', 16225, 1, sleep=delays.append)
        # Same completed run, release-publish only: reuse, never a rebuild dispatch.
        self.assertEqual(client.dispatches(), [{'ref': 'main', 'inputs': {
            'platform': 'macos', 'run_id': '16225', 'attempt': '2'}}])
        self.assertTrue(all(path == DISPATCH for method, path, _ in client.calls if method == 'POST'))
        self.assertEqual(delays, [publication.RETRY_DELAY_SECONDS])

    def test_transient_failure_publishes_on_a_retry_and_stops(self):
        client = FakeForgejo()
        results, red = run_publication_chain(client, 'macos', 16225, ['failure', 'success'])
        self.assertEqual(results, ['failure', 'success'])
        self.assertFalse(red)
        self.assertEqual(len(client.dispatches()), 1)
        self.assertFalse(any(path.startswith('/issues') for _, path, _ in client.calls))

    def test_successful_publication_is_never_repeated(self):
        for result in ('success', 'cancelled', 'skipped'):
            with self.subTest(result=result):
                client = FakeForgejo()
                results, red = run_publication_chain(client, 'macos', 16225, [result])
                self.assertEqual(results, [result])
                self.assertFalse(red)
                self.assertEqual(client.calls, [])

    def test_retries_are_bounded_and_the_last_failure_is_visible(self):
        client = FakeForgejo()
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / 'summary.md'
            original = publication.retry

            def retry(*args, **kwargs):
                return original(*args, summary=summary, run_url='https://ci/run/9', **kwargs)
            publication.retry = retry
            try:
                results, red = run_publication_chain(client, 'macos', 16225,
                                                     ['failure'] * (publication.MAX_PUBLICATION_ATTEMPTS + 2))
            finally:
                publication.retry = original
            self.assertIn('failed 3 times', summary.read_text())
            self.assertIn('could not be read', summary.read_text())
        self.assertEqual(results, ['failure'] * publication.MAX_PUBLICATION_ATTEMPTS)
        self.assertTrue(red)
        self.assertEqual([d['inputs'].get('attempt') for d in client.dispatches()], ['2', '3'])
        created = [body for method, path, body in client.calls if method == 'POST' and path == '/issues']
        self.assertEqual(len(created), 1)
        self.assertEqual(created[0]['title'], publication.FAILED_ISSUE_TITLE)
        self.assertIn('run 16225', created[0]['body'])
        self.assertIn('https://ci/run/9', created[0]['body'])

    def test_exhausted_retry_comments_on_the_open_issue(self):
        client = FakeForgejo(issues=[{'number': 77, 'title': publication.FAILED_ISSUE_TITLE}])
        with self.assertRaises(SystemExit):
            publication.retry(client, 'windows', 38712, publication.MAX_PUBLICATION_ATTEMPTS,
                              sleep=lambda _: None, feeds=Feeds())
        self.assertEqual(client.dispatches(), [])
        self.assertEqual([path for method, path, _ in client.calls if method == 'POST'],
                         ['/issues/77/comments'])

    def test_issue_api_failure_still_ends_red(self):
        class Broken(FakeForgejo):
            def call(self, method, path, body=None):
                if path.startswith('/issues'):
                    raise SystemExit('GET /issues: 500')
                return super().call(method, path, body)
        with self.assertRaises(SystemExit) as raised:
            publication.retry(Broken(), 'macos', 1, publication.MAX_PUBLICATION_ATTEMPTS, feeds=Feeds())
        self.assertIn('no further automatic retry', str(raised.exception))

    def test_dispatch_survives_a_transient_api_error_and_keeps_the_original_payload(self):
        client = FakeForgejo(fail_posts=1)
        publication.dispatch_publication(client, 'macos', 16225, sleep=lambda _: None)
        self.assertEqual(client.dispatches(), [{'ref': 'main', 'inputs': {'platform': 'macos', 'run_id': '16225'}}] * 2)
        client = FakeForgejo(fail_posts=publication.DISPATCH_ATTEMPTS)
        with self.assertRaises(SystemExit):
            publication.dispatch_publication(client, 'macos', 16225, sleep=lambda _: None)

    def test_invalid_requests_are_refused(self):
        for args in [('ios', 1, 1), ('macos', 0, 1), ('macos', 1, 0)]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                publication.retry(FakeForgejo(), *args, sleep=lambda _: None)

    def test_workflow_retries_only_a_failed_publish_job(self):
        workflow = release_publish_jobs()
        self.assertEqual(workflow['on']['workflow_dispatch']['inputs']['attempt']['default'], '1')
        job = workflow['jobs']['retry']
        self.assertEqual(job['needs'], 'publish')
        self.assertIn("needs.publish.result == 'failure'", job['if'])
        self.assertIn('always()', job['if'])
        self.assertIn("github.ref == 'refs/heads/main'", job['if'])
        self.assertNotIn('container', job)
        self.assertEqual(job['permissions']['issues'], 'write')
        self.assertEqual(job['permissions']['actions'], 'write')
        step = next(s for s in job['steps'] if 'publication.py retry' in s.get('run', ''))
        self.assertEqual(step['run'], 'python3 scripts/releases/publication.py retry "$PLATFORM" '
                                      '--run "$SOURCE_RUN" --attempt "$ATTEMPT"')
        self.assertEqual(step['env']['SOURCE_RUN'], '${{ inputs.run_id }}')
        self.assertEqual(step['env']['ATTEMPT'], "${{ inputs.attempt || '1' }}")
        # No compiler, signing or feed credentials in the retry job.
        text = yaml.safe_dump(job)
        for forbidden in ('R2_', 'GPG', 'SPARKLE', 'cargo', 'MIRROR_TOKEN'):
            self.assertNotIn(forbidden, text)
        # A refresh failure after a successful publication is never retried:
        # refresh is its own job and the retry depends on the publish job only.
        publish = workflow['jobs']['publish']
        self.assertFalse(any('releases.yml/dispatches' in s.get('run', '') for s in publish['steps']))
        refresh = workflow['jobs']['refresh']
        self.assertEqual(refresh['needs'], 'publish')
        self.assertNotIn('always()', refresh['if'])
        self.assertTrue(any("'/actions/workflows/releases.yml/dispatches'" in s.get('run', '')
                            for s in refresh['steps']))
        self.assertTrue(all(name in ('publish', 'refresh', 'retry') for name in workflow['jobs']))
        self.assertNotIn('refresh', job['needs'])
        # Retry timeout covers the longest backoff with checkout to spare.
        backoff = publication.RETRY_DELAY_SECONDS * (publication.MAX_PUBLICATION_ATTEMPTS - 1)
        self.assertGreater(int(job['timeout-minutes']) * 60, backoff + 120)



class FeedState(unittest.TestCase):
    """The exhausted-retry issue states what the feed serves, not an assumption."""

    def state(self, head, index=100):
        return publication.feed_state(FakeForgejo(index=index), 'macos', 7, Feeds(head))

    def test_previous_build(self):
        self.assertIn('still serves previous build 5099', self.state((5099, 'a' * 40)))

    def test_this_build_is_already_served(self):
        state = self.state((5100, 'b' * 40))
        self.assertIn('already serves this build 5100', state)
        self.assertNotIn('previous', state)

    def test_newer_build(self):
        self.assertIn('serves newer build 5101', self.state((5101, 'c' * 40)))

    def test_unreadable_feed(self):
        self.assertIn('could not be read', self.state((5100, 'b' * 40), index=None))

    def test_exhausted_issue_carries_the_feed_state(self):
        client = FakeForgejo(index=100)
        with self.assertRaises(SystemExit) as raised:
            publication.retry(client, 'macos', 7, publication.MAX_PUBLICATION_ATTEMPTS,
                              sleep=lambda _: None, feeds=Feeds((5100, 'b' * 40)))
        self.assertIn('already serves this build 5100', str(raised.exception))
        created = [body for method, path, body in client.calls if method == 'POST' and path == '/issues']
        self.assertIn('already serves this build 5100', created[0]['body'])
        self.assertNotIn('previous build', created[0]['body'])


SOURCE = '1' * 40
TREE = '2' * 40
SIGNATURE = 'c2lnbmF0dXJl'


class Crash(Exception):
    pass


class Store:
    """In-memory R2 bucket; `crash_on` makes one put fail as a dying publisher would."""

    def __init__(self, crash_on=None):
        self.objects = {}
        self.puts = []
        self.crash_on = crash_on

    def call(self, method, key, data=b'', content_type=None, cache=None):
        if method != 'GET':
            raise AssertionError(method)
        return self.objects.get(key)

    def put(self, key, data, content_type, cache=None):
        if key == self.crash_on:
            self.crash_on = None
            raise Crash(key)
        self.puts.append(key)
        self.objects[key] = data


class Releases:
    """Forgejo releases/assets double with the calls release.py makes."""

    def __init__(self):
        self.releases = {}
        self.log = []

    def release(self, tag):
        return self.releases.get(tag)

    def call(self, method, path, body=None, content_type='application/json', raw=False):
        self.log.append((method, path))
        if method == 'POST' and path == '/releases':
            release = {'id': len(self.releases) + 1, 'tag_name': body['tag_name'],
                       'target_commitish': body['target_commitish'], 'body': body['body'], 'assets': []}
            self.releases[body['tag_name']] = release
            return release
        if method == 'DELETE':
            for release in self.releases.values():
                release['assets'] = [a for a in release['assets'] if f"/assets/{a['id']}" not in path]
            return None
        raise AssertionError((method, path))

    def upload(self, release, name, data):
        self.log.append(('UPLOAD', release['tag_name'], name))
        release['assets'].append({'id': len(self.log), 'name': name, 'data': data})

    def download(self, tag, name):
        self.log.append(('DOWNLOAD', tag, name))
        return next(a['data'] for a in self.releases[tag]['assets'] if a['name'] == name)


class ResumablePublication(unittest.TestCase):
    """release.py publish of a build an earlier attempt started (#1559)."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        notes = self.root / 'notes.json'
        notes.write_text(json.dumps(dict(build=5100, source=SOURCE, platform='macos', channel='beta',
                                         previous_build=5099, previous_source='0' * 40, items=[])))
        environ = patch.dict(os.environ, {'OKILUM_RELEASE_NOTES': str(notes)})
        environ.start()
        self.addCleanup(environ.stop)
        self.forgejo, self.store = Releases(), Store()

    def args(self, build=5100, source=SOURCE, archive=b'zip-bytes', dmg=b'dmg-bytes'):
        (self.root / 'Okilum.zip').write_bytes(archive)
        (self.root / 'Okilum.dmg').write_bytes(dmg)
        return types.SimpleNamespace(app='okilum', archive=str(self.root / 'Okilum.zip'),
                                     dmg=str(self.root / 'Okilum.dmg'), build=build,
                                     short_version='0.1.' + str(build), source=source, tree=TREE,
                                     signature=SIGNATURE, channel='beta')

    def publish(self, args=None):
        with patch.object(release, 'Forgejo', return_value=self.forgejo), \
                patch.object(release, 'R2', return_value=self.store), \
                patch('builtins.print'):
            release.publish(args or self.args())

    def appcast_builds(self):
        raw = self.store.objects['okilum/appcast.xml']
        return [item.findtext(release_notes.SPARKLE + 'version')
                for item in ET.fromstring(raw).findall('./channel/item')]

    def assert_complete(self):
        self.assertEqual(self.appcast_builds(), ['5100'])
        assets = self.forgejo.releases['macos-stable-5100']['assets']
        self.assertEqual(sorted(a['name'] for a in assets), ['Okilum.dmg', 'Okilum.zip'])
        self.assertEqual(self.store.objects['okilum/macos/beta/latest.zip'], b'zip-bytes')
        self.assertEqual(self.store.objects['okilum/macos/beta/Okilum.dmg'], b'dmg-bytes')
        self.assertEqual(json.loads(self.store.objects[f'okilum/releases/{SOURCE}/macos.json'])['build'], 5100)

    def test_normal_publication_order_is_unchanged(self):
        self.publish()
        self.assertEqual(self.forgejo.log, [('POST', '/releases'),
                                            ('UPLOAD', 'macos-stable-5100', 'Okilum.zip'),
                                            ('UPLOAD', 'macos-stable-5100', 'Okilum.dmg')])
        self.assertEqual(self.store.puts, ['okilum/5100/Okilum.zip', 'okilum/appcast.xml',
                                           'okilum/5100/Okilum.dmg', f'okilum/releases/{SOURCE}/macos.json',
                                           'okilum/macos/beta/latest.zip', 'okilum/macos/beta/Okilum.dmg'])
        self.assert_complete()

    def test_crash_between_release_and_appcast_write_is_completed_by_the_retry(self):
        self.store.crash_on = 'okilum/appcast.xml'
        with self.assertRaises(Crash):
            self.publish()
        self.assertIn('macos-stable-5100', self.forgejo.releases)
        self.assertNotIn('okilum/appcast.xml', self.store.objects)
        self.publish()
        self.assert_complete()
        # The retry made no second release and no duplicate upload of the zip.
        self.assertEqual(self.forgejo.log.count(('POST', '/releases')), 1)
        self.assertEqual(self.forgejo.log.count(('UPLOAD', 'macos-stable-5100', 'Okilum.zip')), 1)

    def test_crash_after_the_appcast_write_is_completed_by_the_retry(self):
        self.store.crash_on = f'okilum/releases/{SOURCE}/macos.json'
        with self.assertRaises(Crash):
            self.publish()
        appcast = self.store.objects['okilum/appcast.xml']
        self.publish()
        self.assert_complete()
        self.assertEqual(self.store.objects['okilum/appcast.xml'], appcast)
        self.assertNotIn('okilum/appcast.xml', self.store.puts[self.store.puts.index('okilum/appcast.xml') + 1:])

    def test_second_publication_of_the_feed_head_is_a_verified_no_op(self):
        self.publish()
        appcast, log = self.store.objects['okilum/appcast.xml'], list(self.forgejo.log)
        self.publish()
        self.assert_complete()
        self.assertEqual(self.store.objects['okilum/appcast.xml'], appcast)
        added = self.forgejo.log[len(log):]
        self.assertNotIn(('POST', '/releases'), added)
        self.assertFalse([entry for entry in added if entry[0] == 'UPLOAD'])
        self.assertIn(('DOWNLOAD', 'macos-stable-5100', 'Okilum.zip'), added)

    def test_resume_syncs_a_mirror_the_crash_left_stale(self):
        self.forgejo.releases['macos-stable'] = {'id': 99, 'tag_name': 'macos-stable', 'assets': []}
        self.publish()
        mirror = self.forgejo.releases['macos-stable']['assets']
        mirror[0]['data'] = b'stale'
        self.publish()
        self.assertEqual([a['data'] for a in self.forgejo.releases['macos-stable']['assets']],
                         [self.store.objects['okilum/appcast.xml']])

    def test_conflicting_existing_release_or_asset_is_refused(self):
        self.publish()
        with self.assertRaises(SystemExit) as raised:
            self.publish(self.args(source='3' * 40))
        self.assertIn('already exists', str(raised.exception))
        with self.assertRaises(SystemExit) as raised:
            self.publish(self.args(archive=b'other-bytes'))
        self.assertIn('different Okilum.zip', str(raised.exception))

    def test_older_build_is_never_resumed_over_a_newer_head(self):
        self.publish()
        newer = ('<item><title>Okilum 0.1.5101</title><sparkle:version>5101</sparkle:version>'
                 '<sparkle:channel>beta</sparkle:channel></item>')
        self.store.objects['okilum/appcast.xml'] = self.store.objects['okilum/appcast.xml'].decode().replace(
            '<title>Okilum updates</title>', '<title>Okilum updates</title>' + newer, 1).encode()
        puts = len(self.store.puts)
        with self.assertRaises(SystemExit) as raised:
            self.publish()
        self.assertIn('Newer build 5101', str(raised.exception))
        self.assertNotIn('okilum/macos/beta/latest.zip', self.store.puts[puts:])
        self.assertNotIn('okilum/appcast.xml', self.store.puts[puts:])


class ResumableNotes(unittest.TestCase):
    def test_recorded_notes_of_this_build_are_reused(self):
        store = Store()
        recorded = dict(build=5100, source=SOURCE, platform='macos', channel='beta',
                        previous_build=5099, previous_source='0' * 40, items=[{'pr': 1}])
        store.objects[release_notes.key(SOURCE, 'macos', 'beta')] = json.dumps(recorded).encode()
        with patch.object(release_notes, 'prepare') as prepare:
            self.assertEqual(publication.resumable_notes(store, None, 5100, SOURCE), recorded)
            prepare.assert_not_called()

    def test_a_build_already_at_the_feed_head_never_becomes_its_own_predecessor(self):
        store = Store()
        items = ''.join(f'<item><sparkle:version>{build}</sparkle:version><sparkle:channel>beta'
                        f'</sparkle:channel><okilum:source>{source}</okilum:source></item>'
                        for build, source in [(5100, SOURCE), (5099, '0' * 40)])
        store.objects['okilum/appcast.xml'] = (
            '<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" '
            'xmlns:okilum="https://git.oklabs.uk/BeFeast/okilum/update-metadata"><channel>'
            + items + '</channel></rss>').encode()
        with patch.object(release_notes, 'merged_items', return_value=[]) as merged:
            notes = publication.resumable_notes(store, None, 5100, SOURCE)
        self.assertEqual((notes['previous_build'], notes['previous_source']), (5099, '0' * 40))
        merged.assert_called_with(None, '0' * 40, SOURCE)

    def test_first_publication_keeps_the_feed_head_as_predecessor(self):
        store = Store()
        store.objects['okilum/appcast.xml'] = (
            '<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" '
            'xmlns:okilum="https://git.oklabs.uk/BeFeast/okilum/update-metadata"><channel><item>'
            '<sparkle:version>5099</sparkle:version><sparkle:channel>beta</sparkle:channel>'
            '<okilum:source>' + '0' * 40 + '</okilum:source></item></channel></rss>').encode()
        with patch.object(release_notes, 'merged_items', return_value=[]):
            notes = publication.resumable_notes(store, None, 5100, SOURCE)
        self.assertEqual(notes['previous_build'], 5099)


if __name__ == '__main__':
    unittest.main()
