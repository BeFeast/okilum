import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import catalog
import publish as p

SOURCE = 'a' * 40


class Store:
    def __init__(self):
        self.data = {}
        self.writes = []

    def call(self, method, key):
        assert method == 'GET'
        return self.data.get(key)

    def put(self, key, data, *_):
        self.data[key] = data
        self.writes.append(key)


def fixture(store):
    names = {'macos': ['Tessera-macos.zip'], 'windows': ['Setup.exe', 'Tessera-windows-portable.zip'],
             'linux': ['tessera-0.1.702-1-x86_64.pkg.tar.zst', 'tessera-0.1.702-1-x86_64.pkg.tar.zst.sig']}
    for offset, (platform, filenames) in enumerate(names.items()):
        assets = []
        for name in filenames:
            key = f'archive/{name}'
            data = name.encode()
            store.data[key] = data
            assets.append(catalog.asset(key, name, data))
        catalog.record(store, platform, 700 + offset, SOURCE, assets)
    store.data['tessera/appcast.xml'] = f'''<rss xmlns:s="{p.appcast.SPARKLE}" xmlns:t="{p.appcast.TESSERA}">
      <channel><item><s:version>700</s:version><s:channel>beta</s:channel><t:source>{SOURCE}</t:source></item></channel></rss>'''.encode()
    store.data['tessera/windows/beta/releases.beta.json'] = catalog.encode(
        {'Assets': [{'Type': 'Full', 'Version': '0.1.701'}]})
    store.data['tessera/windows/builds/701/release.json'] = catalog.encode({'build': 701, 'source': SOURCE})
    store.data['tessera/arch/beta/x86_64/latest.json'] = catalog.encode({'build': 702, 'source': SOURCE})
    store.writes.clear()
    return catalog.bundle(store, SOURCE, 700)


class CatalogTests(unittest.TestCase):
    def test_same_source_different_platform_numbers(self):
        store = Store()
        release = fixture(store)
        self.assertEqual([v['build'] for v in release['platforms'].values()], [700, 701, 702])
        files = catalog.download(store, release)
        self.assertEqual(len(files), 6)
        for line in files['SHA256SUMS'].decode().splitlines():
            digest, name = line.split('  ')
            self.assertEqual(digest, hashlib.sha256(files[name]).hexdigest())
        self.assertEqual(p.choose(store, 700), release)

    def test_missing_platform_never_promotes_or_mirrors(self):
        store = Store()
        fixture(store)
        del store.data[f'{catalog.PREFIX}/{SOURCE}/windows.json']
        with patch.object(p, 'mirror_tag') as mirror:
            p.execute(store, None, None)
            with self.assertRaisesRegex(ValueError, 'lacks completed'):
                p.execute(store, None, None, 700)
            mirror.assert_not_called()
        self.assertEqual(store.writes, [])

    def test_mixed_source_and_corruption_fail_before_mutation(self):
        store = Store()
        fixture(store)
        key = f'{catalog.PREFIX}/{SOURCE}/windows.json'
        original = store.data[key]
        bad = json.loads(original)
        bad['source'] = 'b' * 40
        store.data[key] = catalog.encode(bad)
        with self.assertRaisesRegex(ValueError, 'Mixed source'):
            p.execute(store, None, None, 700)
        store.data[key] = original
        store.data['archive/Setup.exe'] += b'corrupt'
        with self.assertRaisesRegex(ValueError, 'verification failed'):
            p.execute(store, None, None, 700)
        self.assertEqual(store.writes, [])

    def test_beta_does_not_promote_and_retry_is_noop(self):
        store = Store()
        release = fixture(store)
        with patch.object(p, 'mirror_tag', return_value='beta') as mirror, \
             patch.object(p, 'notes', return_value='Notes'), \
             patch.object(p, 'github_release') as github, \
             patch.object(p, 'preflight_stable') as preflight:
            p.execute(store, None, None)
            from unittest.mock import Mock
            api = Mock()
            api.call.return_value = {'tag_name': 'beta'}
            p.execute(store, api, None)
            mirror.assert_called_once_with(release, False)
            github.assert_called_once()
            preflight.assert_not_called()
            self.assertEqual(store.writes, [f'{catalog.PREFIX}/beta.json'])

    def test_completed_marker_with_missing_beta_release_is_repaired(self):
        from unittest.mock import Mock
        store = Store()
        release = fixture(store)
        store.data[f'{catalog.PREFIX}/beta.json'] = catalog.encode(release)
        api = Mock()
        api.call.return_value = None
        with patch.object(p, 'mirror_tag', return_value='beta'), \
             patch.object(p, 'notes', return_value='Notes'), \
             patch.object(p, 'github_release') as publish:
            p.execute(store, api, None)
            publish.assert_called_once()

    def test_upload_failure_retries_same_beta(self):
        store = Store()
        fixture(store)
        with patch.object(p, 'mirror_tag', return_value='beta'), \
             patch.object(p, 'notes', return_value='Notes'), \
             patch.object(p, 'github_release', side_effect=OSError('upload failed')):
            with self.assertRaises(OSError):
                p.execute(store, None, None)
        self.assertNotIn(f'{catalog.PREFIX}/beta.json', store.data)

    def test_interrupted_stable_cannot_switch_selection(self):
        store = Store()
        release = fixture(store)
        store.data[f'{catalog.PREFIX}/promoting.json'] = catalog.encode(release)
        with self.assertRaisesRegex(ValueError, 'Finish the interrupted'):
            p.execute(store, None, None, 701)
        self.assertEqual(store.writes, [])

    def test_explicit_recovery_validates_newer_build_before_replacing_pending(self):
        store = Store()
        release = fixture(store)
        pending = {**release, 'build': 699}
        store.data[f'{catalog.PREFIX}/promoting.json'] = catalog.encode(pending)
        with patch.object(p, 'notes', return_value='Notes'), \
             patch.object(p, 'preflight_stable', side_effect=ValueError('bad archive')) as preflight:
            with self.assertRaisesRegex(ValueError, 'bad archive'):
                p.execute(store, None, None, 700, supersede_pending=True)
            preflight.assert_called_once_with(store, release)
        self.assertEqual(json.loads(store.data[f'{catalog.PREFIX}/promoting.json']), pending)
        self.assertEqual(store.writes, [])
        with self.assertRaisesRegex(ValueError, 'newer build'):
            p.execute(store, None, None, 698, supersede_pending=True)

    def test_completed_stable_retry_is_noop(self):
        store = Store()
        release = fixture(store)
        for name in ('promoting', 'stable'):
            store.data[f'{catalog.PREFIX}/{name}.json'] = catalog.encode(release)
        with patch.object(p, 'mirror_tag') as mirror:
            p.execute(store, None, None, 700)
            mirror.assert_not_called()
        self.assertEqual(store.writes, [])

    def test_no_rollback(self):
        store = Store()
        release = fixture(store)
        release['build'] = 703
        store.data[f'{catalog.PREFIX}/stable.json'] = catalog.encode(release)
        with self.assertRaisesRegex(ValueError, 'rollback'):
            p.execute(store, None, None, 700)
        self.assertEqual(store.writes, [])

    def test_stable_updates_all_platforms_then_records_completion(self):
        store = Store()
        release = fixture(store)
        with patch.object(p, 'notes', return_value='Notes'), \
             patch.object(p, 'preflight_stable'), \
             patch.object(p, 'mirror_tag', return_value='v0.1.700') as mirror, \
             patch.object(p.arch, 'SigningKey'), \
             patch.object(p.arch, 'validate_package'), \
             patch.object(p.arch, 'execute') as linux, \
             patch.object(p.windows, 'promote') as windows, \
             patch.object(p, 'promote_macos') as macos, \
             patch.object(p, 'github_release') as github:
            p.execute(store, None, None, 700)
            mirror.assert_called_once_with(release, True)
            self.assertEqual(linux.call_args.args[0].build, 702)
            windows.assert_called_once_with(701, store)
            self.assertEqual(macos.call_args.args[0].build, 700)
            self.assertTrue(github.call_args.args[-1])
            self.assertEqual(store.writes[0], f'{catalog.PREFIX}/promoting.json')
            self.assertEqual(store.writes[-1], f'{catalog.PREFIX}/stable.json')
            self.assertEqual(store.data['tessera/macos/latest.zip'], b'Tessera-macos.zip')

    def test_catalog_rejects_reused_build_and_path_injection(self):
        store = Store()
        release = fixture(store)
        with self.assertRaisesRegex(ValueError, 'Conflicting'):
            catalog.record(store, 'windows', 701, SOURCE, [])
        release['platforms']['macos']['assets'][0]['name'] = '../escape'
        with self.assertRaisesRegex(ValueError, 'unsafe'):
            catalog.download(store, release)

    def test_windows_portable_is_archived_in_catalog(self):
        store = Store()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name = 'BeFeast.Tessera-0.1.701-beta-full.nupkg'
            data = b'package'
            (root / name).write_bytes(data)
            feed = {'Assets': [{'FileName': name, 'PackageId': 'BeFeast.Tessera', 'Type': 'Full',
                               'Version': '0.1.701', 'Size': len(data), 'SHA256': hashlib.sha256(data).hexdigest()}]}
            (root / 'releases.beta.json').write_text(json.dumps(feed))
            (root / 'BeFeast.Tessera-beta-Setup.exe').write_bytes(b'MZsetup')
            portable = root / 'portable.zip'
            portable.write_bytes(b'PKportable')
            p.windows.publish(root, 701, SOURCE, store, portable)
            entry = json.loads(store.data[f'{catalog.PREFIX}/{SOURCE}/windows.json'])
            for a in entry['assets']:
                self.assertEqual(hashlib.sha256(store.data[a['key']]).hexdigest(), a['sha256'])
            self.assertEqual([a['name'] for a in entry['assets']], ['Setup.exe', 'Tessera-windows-portable.zip'])


class GitHubTests(unittest.TestCase):
    def test_delete_asset_uses_github_documented_endpoint(self):
        from unittest.mock import Mock
        response = Mock()
        response.__enter__ = Mock(return_value=response)
        response.__exit__ = Mock(return_value=False)
        response.read.return_value = b''
        with patch.dict('os.environ', {'MIRROR_TOKEN': 'test-token'}), \
             patch.object(p.urllib.request, 'urlopen', return_value=response) as request:
            p.GitHub().call('DELETE', '/releases/assets/9')
        sent = request.call_args.args[0]
        self.assertEqual(sent.full_url, 'https://api.github.com/repos/BeFeast/tessera/releases/assets/9')
        self.assertEqual(sent.method, 'DELETE')

    def test_rolling_release_reuses_id_and_publishes_after_all_uploads(self):
        calls = []
        class API:
            def call(self, method, path, body=None, **kwargs):
                calls.append((method, path, body))
                if method == 'GET' and '/tags/' in path:
                    return {'id': 42, 'tag_name': 'beta', 'draft': False, 'prerelease': True,
                            'assets': [{'name': 'x.zip'}, {'name': 'SHA256SUMS'}]}
                if method == 'GET':
                    return [{'id': 42, 'tag_name': 'beta', 'draft': True,
                             'assets': [{'id': 9, 'name': 'obsolete.zip'}]}]
        p.github_release(API(), 'beta', {'build': 700, 'source': SOURCE}, {'x.zip': b'ZIP', 'SHA256SUMS': b'hash'}, 'notes', False)
        self.assertEqual(calls[1], ('PATCH', '/releases/42', {'draft': True}))
        self.assertEqual(calls[2][:2], ('DELETE', '/releases/assets/9'))
        self.assertFalse(calls[-2][2]['draft'])
        self.assertTrue(calls[-2][2]['prerelease'])
        self.assertNotIn('make_latest', calls[-2][2])
        self.assertFalse(any(c[0] == 'POST' and c[1] == '/releases' for c in calls))
    def test_untagged_beta_recovery_publishes_explicit_tag_and_checks_result(self):
        calls = []
        class API:
            def call(self, method, path, body=None, **kwargs):
                calls.append((method, path, body))
                if method == 'GET' and '/tags/' not in path:
                    return [{'id': 42, 'name': 'Beta', 'prerelease': True,
                             'tag_name': 'untagged-old', 'assets': []}]
                if method == 'GET':
                    return None
        with self.assertRaisesRegex(ValueError, 'requested tag/assets'):
            p.github_release(API(), 'beta', {'build': 700, 'source': SOURCE}, {}, 'notes', False)
        self.assertFalse(any(c[0] == 'POST' and c[1] == '/releases' for c in calls))
        publish = calls[-2][2]
        self.assertEqual(publish['tag_name'], 'beta')
        self.assertEqual(publish['target_commitish'], SOURCE)

    def test_duplicate_temporary_betas_removed_only_after_verified_publication(self):
        calls = []
        class API:
            def call(self, method, path, body=None, **kwargs):
                calls.append((method, path, body))
                if method == 'GET' and '/tags/' not in path:
                    return [{'id': n, 'name': 'Beta', 'tag_name': f'untagged-{n}',
                             'prerelease': True, 'assets': []} for n in [43, 42]]
                if method == 'GET':
                    return {'id': 42, 'tag_name': 'beta', 'draft': False,
                            'prerelease': True, 'assets': []}
        p.github_release(API(), 'beta', {'build': 700, 'source': SOURCE}, {}, 'notes', False)
        self.assertEqual(calls[-2][:2], ('GET', '/releases/tags/beta'))
        self.assertEqual(calls[-1][:2], ('DELETE', '/releases/43'))
        self.assertFalse(any(c[0] == 'DELETE' and c[1] == '/releases/42' for c in calls))

    def test_draft_retry_reuses_release_and_sets_latest_after_publication(self):
        calls = []
        class API:
            def call(self, method, path, body=None, **kwargs):
                calls.append((method, path, body))
                if method == 'GET' and '/tags/' in path:
                    return {'id': 7, 'tag_name': 'v0.1.700', 'draft': False, 'prerelease': False,
                            'assets': [{'name': 'x.zip'}]}
                if method == 'GET':
                    return [{'id': 7, 'tag_name': 'v0.1.700', 'draft': True, 'assets': []}]
        p.github_release(API(), 'v0.1.700', {'build': 700, 'source': SOURCE}, {'x.zip': b'ZIP'}, 'notes', True)
        self.assertFalse(any(c[0] == 'POST' and c[1] == '/releases' for c in calls))
        self.assertEqual(calls[-1], ('PATCH', '/releases/7', {'make_latest': 'true'}))
        self.assertFalse(calls[-3][2]['draft'])
        self.assertNotIn('make_latest', calls[-3][2])



if __name__ == '__main__':
    unittest.main()

class RollingBetaTests(unittest.TestCase):
    def test_latest_platform_sources_can_differ_but_stable_cannot(self):
        store = Store()
        fixture(store)
        source = 'b' * 40
        old = json.loads(store.data[f'{catalog.PREFIX}/{SOURCE}/windows.json'])
        old.update(source=source, build=703)
        store.data[f'{catalog.PREFIX}/{source}/windows.json'] = catalog.encode(old)
        store.data['tessera/windows/beta/releases.beta.json'] = catalog.encode(
            {'Assets': [{'Type': 'Full', 'Version': '0.1.703'}]})
        store.data['tessera/windows/builds/703/release.json'] = catalog.encode({'build': 703, 'source': source})
        release = p.choose_beta(store)
        self.assertEqual(release['platforms']['windows']['source'], source)
        self.assertEqual(release['platforms']['windows']['build'], 703)
        self.assertEqual(p.choose(store, 700)['platforms']['windows']['build'], 701)
        self.assertIn(source, p.beta_notes(release))
        # A completed rerun is not a published channel head.
        old['build'] = 704
        store.data[f'{catalog.PREFIX}/{source}/windows.json'] = catalog.encode(old)
        with self.assertRaisesRegex(ValueError, 'channel/catalog mismatch'):
            p.choose_beta(store)

    def test_each_platform_has_rollback_guard_before_any_write(self):
        store = Store()
        release = fixture(store)
        release['platforms']['windows']['build'] = 999
        store.data[f'{catalog.PREFIX}/beta.json'] = catalog.encode(release)
        with patch.object(p, 'mirror_tag') as mirror:
            with self.assertRaisesRegex(ValueError, 'windows beta rollback'):
                p.execute(store, None, None)
            mirror.assert_not_called()
        self.assertEqual(store.writes, [])
