"""Offline Forgejo body regressions: canonical notes, bytes and promotion retries."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

import publish as coordinator
import release as macos
import release_notes as notes
from test_mac_only import prepared
from test_publish import Store
from test_release_notes import feed, pr

SOURCE = 'a' * 40
ITEMS = [{'pr': 42, 'kind': 'feature', 'text': 'Jump to projects'},
         {'pr': 43, 'kind': 'fix', 'text': 'Keep the cursor in its note (#2452)'}]


def payload(platform='macos', channel='beta', build=700, items=None):
    return dict(build=build, source=SOURCE, platform=platform, channel=channel,
                previous_build=600, previous_source='b' * 40,
                items=ITEMS if items is None else items)


def footer(channel='beta'):
    return (f'Signed and notarized macOS arm64 build 700 from {SOURCE}.\n'
            f'Channel at publication: {channel}.')


class Forgejo:
    """In-memory API; a call outside the expected release path fails the test."""
    def __init__(self, body=None):
        self.current = None if body is None else {'id': 9, 'body': body}
        self.calls, self.uploads = [], []

    def release(self, tag):
        if tag == 'macos-stable':
            return None
        assert tag == 'macos-stable-700', tag
        return self.current

    def call(self, method, path, body):
        self.calls.append((method, path, body))
        if (method, path) == ('POST', '/releases'):
            assert self.current is None
            self.current = {'id': 9, **body}
        elif (method, path) == ('PATCH', '/releases/9'):
            self.current.update(body)
        else:
            raise AssertionError((method, path))
        return self.current

    def upload(self, release, name, data):
        self.uploads.append((release['id'], name, data))


class Offline(unittest.TestCase):
    def setUp(self):
        guard = patch('urllib.request.urlopen', side_effect=AssertionError('Fixtures must not use a network'))
        guard.start(); self.addCleanup(guard.stop)


class Body(Offline):
    def test_same_items_and_labels_as_whats_new_for_each_platform_and_channel(self):
        for platform in ('macos', 'windows', 'linux-appimage'):
            for channel in ('beta', 'stable'):
                with self.subTest(platform=platform, channel=channel):
                    body = notes.forgejo_body(payload(platform, channel), 'Technical footer')
                    self.assertIn('### New\n\n- Jump to projects', body)
                    self.assertIn('### Fixed\n\n- Keep the cursor', body)
                    for item in ITEMS:
                        self.assertEqual(body.count(f'/pulls/{item["pr"]}'), 1)
                    self.assertIn('[#2452](https://git.oklabs.uk/BeFeast/okilum/issues/2452)', body)
                    self.assertTrue(body.endswith('\n\nTechnical footer'))

    def test_ci_test_docs_only_range_and_empty_range(self):
        internal = [pr(1, 'ci: Cache'), pr(2, 'test(editor): Coverage'),
                    pr(3, 'docs: Instructions')]
        visible = [item for change in internal if (item := notes.pr_item(change))]
        self.assertEqual(visible, [])
        for items in (visible, [{'kind': kind} for kind in ('ci', 'test', 'docs')]):
            body = notes.forgejo_body(payload(items=items), footer())
            self.assertEqual(body, f'{notes.FORGEJO_START}\nбез видимых изменений\n{notes.FORGEJO_END}\n\n{footer()}')
        # Positive control: the same range plus a visible change must render it.
        visible.append(notes.pr_item(pr(4, 'fix: Open the correct note')))
        self.assertIn('Open the correct note', notes.forgejo_body(payload(items=visible), footer()))

    def test_plain_text_stays_inline_and_issue_and_pr_links_are_distinct(self):
        items = [dict(pr=42, kind='fix', text='Keep [cursor] <safe> & *literal*\n#2452')]
        body = notes.forgejo_body(payload(items=items), footer())
        self.assertIn(r'Keep \[cursor\] \<safe\> & \*literal\* [#2452]', body)
        self.assertIn('/issues/2452', body)
        self.assertIn('/pulls/42', body)

    def test_stable_replaces_beta_range_once_and_retains_legacy_footer(self):
        beta = notes.forgejo_body(payload(), footer())
        stable = payload(channel='stable', items=[dict(pr=44, kind='fix', text='Since previous Stable')])
        body = notes.forgejo_body(stable, beta)
        self.assertNotIn('Jump to projects', body)
        self.assertIn('Since previous Stable', body)
        self.assertEqual(body, notes.forgejo_body(stable, footer()))
        self.assertEqual(body, notes.forgejo_body(stable, body))
        self.assertEqual(body.count(notes.FORGEJO_START), 1)
        self.assertEqual(body.count('Channel at publication:'), 1)


class MacPublication(Offline):
    def test_wrong_staged_notes_fail_before_release_or_upload(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'notes.json'
            for field, value in [('build', 701), ('source', 'b' * 40),
                                 ('platform', 'windows'), ('channel', 'stable')]:
                with self.subTest(field=field):
                    path.write_text(json.dumps({**payload(), field: value}))
                    store, forgejo = Store(), Forgejo()
                    with patch.dict(os.environ, OKILUM_RELEASE_NOTES=str(path)), \
                            patch.object(macos, 'R2', return_value=store), \
                            patch.object(macos, 'Forgejo', return_value=forgejo):
                        with self.assertRaisesRegex(ValueError, 'do not belong'):
                            macos.publish(SimpleNamespace(app='okilum', build=700, source=SOURCE,
                                channel='beta', archive=Path(directory) / 'absent.zip'))
                    self.assertEqual((forgejo.calls, forgejo.uploads, store.writes), ([], [], []))

    def test_unstaged_cli_uses_same_generator_before_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / 'signed.zip'; archive.write_bytes(b'original signed archive')
            store, forgejo = Store(), Forgejo()
            with patch.dict(os.environ, OKILUM_RELEASE_NOTES=''), \
                    patch.object(macos, 'R2', return_value=store), \
                    patch.object(macos, 'Forgejo', return_value=forgejo), \
                    patch.object(macos, 'update_appcast'), \
                    patch.object(notes, 'prepare', return_value=payload()) as prepare:
                macos.publish(SimpleNamespace(app='okilum', build=700, source=SOURCE, tree='c' * 40,
                    short_version='0.1.700', channel='beta', signature='original', archive=archive))
            prepare.assert_called_once_with(store, forgejo, 'macos', 700, SOURCE, 'beta')
            self.assertEqual(forgejo.calls[0][2]['body'], notes.forgejo_body(payload(), footer()))

    def test_staged_beta_and_direct_stable_publish_preserve_all_non_body_outputs(self):
        spec = importlib.util.spec_from_file_location('macos_artifact_body_test',
                    Path(__file__).with_name('macos-artifact.py'))
        artifact = importlib.util.module_from_spec(spec); spec.loader.exec_module(artifact)
        for channel in ('beta', 'stable'):
            with self.subTest(channel=channel), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'signed.zip').write_bytes(b'original signed notarized zip')
                (root / 'Okilum-0.1.700.dmg').write_bytes(b'original notarized dmg')
                sidecar = payload(channel=channel)
                (root / 'notes.json').write_text(json.dumps(sidecar))
                metadata = dict(BUILD='700', DISPLAY_VERSION='0.1.700', SOURCE_SHA=SOURCE,
                    SOURCE_TREE='c' * 40, SIGNATURE='original-sparkle-signature',
                    archive='signed.zip', dmg='Okilum-0.1.700.dmg')
                (root / 'release.json').write_text(json.dumps(metadata))
                store = Store(); forgejo = Forgejo()
                env = dict(OKILUM_RELEASE_NOTES=str(root / 'notes.json'), GITHUB_SHA=SOURCE,
                           GITHUB_RUN_NUMBER='-4300')
                with patch.dict(os.environ, env), patch.object(macos, 'R2', return_value=store), \
                        patch.object(macos, 'Forgejo', return_value=forgejo), \
                        patch.object(notes, 'prepare', side_effect=AssertionError('Do not recalculate staged notes')):
                    if channel == 'beta':
                        with patch('sys.argv', ['macos-artifact.py', 'publish', directory]):
                            artifact.main()
                    else:
                        macos.publish(SimpleNamespace(app='okilum', build=700, short_version='0.1.700',
                            source=SOURCE, tree='c' * 40, signature='original-sparkle-signature',
                            channel=channel, archive=root / 'signed.zip', dmg=root / 'Okilum-0.1.700.dmg'))
                post = forgejo.calls[0]
                self.assertEqual(post[:2], ('POST', '/releases'))
                fields = dict(post[2]); body = fields.pop('body')
                self.assertEqual(fields, dict(tag_name='macos-stable-700', target_commitish=SOURCE,
                    name='Okilum 0.1.700 (700)', draft=False, prerelease=channel != 'stable'))
                self.assertEqual(body, notes.forgejo_body(sidecar, footer(channel)))
                self.assertEqual(forgejo.uploads, [(9, name, (root / name).read_bytes())
                    for name in ('signed.zip', 'Okilum-0.1.700.dmg')])
                expected = {'okilum/700/signed.zip': (root / 'signed.zip').read_bytes(),
                            'okilum/700/Okilum-0.1.700.dmg': (root / 'Okilum-0.1.700.dmg').read_bytes(),
                            'okilum/macos/beta/latest.zip': (root / 'signed.zip').read_bytes(),
                            'okilum/macos/beta/Okilum.dmg': (root / 'Okilum-0.1.700.dmg').read_bytes()}
                self.assertEqual(set(store.writes), {*expected, 'okilum/appcast.xml',
                    f'okilum/releases/{SOURCE}/macos.json'})
                for key, data in expected.items():
                    self.assertEqual(store.data[key], data)
                catalog = json.loads(store.data[f'okilum/releases/{SOURCE}/macos.json'])
                self.assertEqual((catalog['platform'], catalog['source'], catalog['build']), ('macos', SOURCE, 700))
                self.assertEqual(catalog['assets'], [coordinator.catalog.asset(key, name, data)
                    for key, name, data in [('okilum/700/signed.zip', 'Okilum-macos.zip', expected['okilum/700/signed.zip']),
                        ('okilum/700/Okilum-0.1.700.dmg', 'Okilum-0.1.700.dmg', expected['okilum/700/Okilum-0.1.700.dmg'])]])
                item = ET.fromstring(store.data['okilum/appcast.xml']).find('./channel/item')
                self.assertEqual(item.find('enclosure').attrib, dict(url='https://updates.befeast.com/okilum/700/signed.zip',
                    length=str(len(expected['okilum/700/signed.zip'])), type='application/octet-stream',
                    **{notes.SPARKLE + 'edSignature': 'original-sparkle-signature', notes.SPARKLE + 'installationType': 'application'}))
                self.assertEqual(item.findtext('description'), notes.description(sidecar))
                self.assertEqual(item.findtext(notes.SPARKLE + 'channel'), channel)

    def test_promotion_and_retry_replace_notes_without_uploading_or_changing_signatures(self):
        store = Store()
        tree = ET.fromstring(feed((700, SOURCE, 'beta')))
        enclosure = dict(url='https://example.invalid/original.zip', length='42',
                         **{notes.SPARKLE + 'edSignature': 'original-signature'})
        ET.SubElement(tree.find('./channel/item'), 'enclosure', enclosure)
        store.data['okilum/appcast.xml'] = ET.tostring(tree)
        beta = notes.forgejo_body(payload(), footer())
        forgejo = Forgejo(beta)
        stable = payload(channel='stable', items=[dict(pr=44, kind='fix', text='Stable range')])
        with patch.object(macos, 'R2', return_value=store), patch.object(macos, 'Forgejo', return_value=forgejo):
            for _ in range(2):
                macos.promote(SimpleNamespace(app='okilum', build=700, release_notes=stable))
        expected = notes.forgejo_body(stable, footer())
        self.assertEqual(forgejo.calls, [('PATCH', '/releases/9', {'prerelease': False, 'body': expected})] * 2)
        self.assertEqual(forgejo.uploads, [])
        self.assertEqual(set(store.writes), {'okilum/appcast.xml'})
        item = ET.fromstring(store.data['okilum/appcast.xml']).find('./channel/item')
        self.assertEqual(item.findtext('description'), notes.description(stable))
        self.assertEqual(item.find('enclosure').attrib, enclosure)

    def test_promotion_without_new_notes_preserves_body(self):
        forgejo = Forgejo(notes.forgejo_body(payload(), footer()))
        before = forgejo.current['body']
        with patch.object(macos, 'Forgejo', return_value=forgejo), patch.object(macos, 'R2'), \
                patch.object(macos, 'update_appcast'):
            macos.promote(SimpleNamespace(app='okilum', build=700))
        self.assertEqual(forgejo.calls, [('PATCH', '/releases/9', {'prerelease': False})])
        self.assertEqual(forgejo.current['body'], before)

    def test_mac_only_stable_uses_persisted_range_and_patches_only_body(self):
        store = prepared(); forgejo = Forgejo(footer())
        with patch.object(notes, 'merged_items', return_value=ITEMS) as select:
            for _ in range(2):
                coordinator.promote_mac_only(store, 700, forgejo=forgejo)
        select.assert_called_once()
        stable = json.loads(store.data[notes.key(SOURCE, 'macos', 'stable')])
        expected = notes.forgejo_body(stable, footer())
        self.assertEqual(forgejo.calls, [('PATCH', '/releases/9', {'body': expected})] * 2)
        self.assertEqual(forgejo.uploads, [])


class OtherPlatforms(Offline):
    def test_windows_beta_and_stable_keep_existing_payloads_without_forgejo_releases(self):
        from test_stable_notes import WindowsStable
        store = Store()
        with patch.object(macos, 'Forgejo', side_effect=AssertionError('Windows does not publish Forgejo releases')):
            WindowsStable().candidate(store)
            before = dict(store.data)
            with patch.dict(os.environ, OKILUM_WINDOWS_STABLE_REQUIRE_SIGNED='1'):
                coordinator.windows.promote(705, store)
        self.assertEqual(store.data['okilum/windows/stable/Setup.exe'],
                         before['okilum/windows/builds/705/Setup.exe'])
        for key, value in before.items():
            if '/stable/' not in key:
                self.assertEqual(store.data[key], value)
        self.assertEqual(store.data['okilum/appcast.xml'], before['okilum/appcast.xml'])

    def test_appimage_beta_stable_and_retry_keep_signed_bytes_without_forgejo_releases(self):
        import appimage_publish
        import appimage_stable
        from test_appimage_publication import Store as AppImageStore, Key, evidence
        receipt, _, image, provenance = evidence()
        store, signing = AppImageStore(), Key()
        client = Mock()
        client.call.side_effect = AssertionError('Only the patched notes inventory may query Forgejo')
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(notes, 'merged_items', return_value=ITEMS):
            root = Path(directory)
            (root / appimage_stable.IMAGE).write_bytes(image)
            (root / 'provenance.json').write_text(json.dumps(provenance))
            appimage_publish.publish(store, signing, root, root, SOURCE, receipt, 9999, client=client)
            before = dict(store.objects)
            for _ in range(2):
                appimage_stable.execute(store, signing, root, SOURCE, 5023, client=client)
            for channel in ('beta', 'stable'):
                sidecar = json.loads(store.objects[notes.key(SOURCE, 'linux-appimage', channel)])
                self.assertEqual(sidecar['items'], ITEMS)
                self.assertIn('Jump to projects', notes.forgejo_body(sidecar, ''))
                for name in (appimage_stable.IMAGE, appimage_stable.IMAGE + '.sig'):
                    self.assertEqual(store.objects[f'okilum/linux/{channel}/{name}'],
                                     before[f'okilum/linux/builds/5023/{name}'])
            for key, value in before.items():
                self.assertEqual(store.objects[key], value)
        client.call.assert_not_called()

