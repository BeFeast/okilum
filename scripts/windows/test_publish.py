import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('windows_publish', Path(__file__).with_name('publish.py'))
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


class Store:
    def __init__(self):
        self.data = {}
        self.writes = []

    def call(self, method, key):
        assert method == 'GET'
        return self.data.get(key)

    def put(self, key, data, content_type, cache=None):
        self.data[key] = data
        self.writes.append(key)


def fixture(root, build=7000):
    name = f'BeFeast.Okilum-0.1.{build}-beta-full.nupkg'
    data = b'package contents'
    (root / name).write_bytes(data)
    feed = {'Assets': [{'PackageId': 'BeFeast.Okilum', 'Version': f'0.1.{build}',
        'Type': 'Full', 'FileName': name, 'Size': len(data),
        'SHA256': hashlib.sha256(data).hexdigest()}]}
    (root / 'releases.beta.json').write_text(json.dumps(feed))
    (root / 'BeFeast.Okilum-beta-Setup.exe').write_bytes(b'MZinstaller')
    return name, feed


class PublicationTests(unittest.TestCase):
    def test_client_contract_matches_published_channel_urls(self):
        from urllib.parse import urlsplit
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            store = Store()
            p.publish(root, 7000, 'source', store)
            p.promote(7000, store)
            self.assertEqual(p.CONTRACT['default_channel'], 'beta')
            for channel in ['beta', 'stable']:
                # windows_feed.rs embeds this same contract; its SDK test captures
                # the real request and checks this path (including releases name).
                url = f"{p.CONTRACT['public_root']}/{p.CONTRACT['prefix']}/{channel}/releases.{channel}.json"
                key = urlsplit(url).path.lstrip('/')
                self.assertIn(key, store.data)
                self.assertEqual(json.loads(store.data[key])['Assets'][0]['Version'], '0.1.7000')

    def test_publish_prepare_promote_preserve_bytes_feed_last(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name, feed = fixture(root)
            store = Store()
            p.publish(root, 7000, 'source', store)
            self.assertEqual(store.writes[-1], 'okilum/windows/beta/releases.beta.json')
            p.prepare(root / 'next', store)
            self.assertEqual((root / 'next' / name).read_bytes(), (root / name).read_bytes())
            p.promote(7000, store)
            self.assertEqual(store.writes[-1], 'okilum/windows/stable/releases.stable.json')
            self.assertEqual(store.data[f'okilum/windows/stable/{name}'], (root / name).read_bytes())
            self.assertEqual(store.data['okilum/windows/stable/Setup.exe'], b'MZinstaller')
            self.assertEqual(json.loads(store.data[store.writes[-1]]), feed)

    def test_archive_only_keeps_beta_and_still_promotes(self):
        """An explicit older commit (#1040): archived, promotable, beta untouched."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'newer').mkdir()
            (root / 'older').mkdir()
            store = Store()
            fixture(root / 'newer', 7100)
            p.publish(root / 'newer', 7100, 'newer-source', store)
            beta = store.data['okilum/windows/beta/releases.beta.json']
            installer = store.data['okilum/windows/beta/Setup.exe']
            name, _ = fixture(root / 'older', 7000)
            (root / 'older' / 'BeFeast.Okilum-beta-Setup.exe').write_bytes(b'MZolder')
            # Control: the ordinary path refuses to roll the feed back.
            with self.assertRaisesRegex(ValueError, 'roll back'):
                p.publish(root / 'older', 7000, 'older-source', store)
            p.publish(root / 'older', 7000, 'older-source', store, archive_only=True)
            self.assertEqual(store.data['okilum/windows/beta/releases.beta.json'], beta)
            self.assertEqual(store.data['okilum/windows/beta/Setup.exe'], installer)
            self.assertEqual(json.loads(store.data['okilum/windows/builds/7000/release.json'])['source'],
                             'older-source')
            p.promote(7000, store)
            self.assertEqual(store.data['okilum/windows/stable/Setup.exe'], b'MZolder')
            self.assertEqual(store.data[f'okilum/windows/stable/{name}'], (root / 'older' / name).read_bytes())

    def test_corruption_and_traversal_never_publish_feed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            name, feed = fixture(root)
            (root / name).write_bytes(b'corrupt')
            store = Store()
            with self.assertRaises(ValueError): p.publish(root, 7000, 'source', store)
            self.assertEqual(store.writes, [])
            feed['Assets'][0]['FileName'] = '../escape.nupkg'
            with self.assertRaises(ValueError): p.validate_feed(feed, lambda _: self.fail('must not read'))

    def test_no_implicit_downgrade_or_wrong_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root, 7001)
            store = Store()
            p.publish(root, 7001, 'source', store)
            p.promote(7001, store)
            fixture(root, 7000)
            with self.assertRaises(ValueError): p.publish(root, 7000, 'source', store)
            with self.assertRaises(ValueError): p.publish(root, 7002, 'source', store)

    def test_missing_previous_feed_is_first_release(self):
        with tempfile.TemporaryDirectory() as directory:
            p.prepare(Path(directory), Store())
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_corrupt_installer_cannot_promote(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            store = Store()
            p.publish(root, 7000, 'source', store)
            store.data['okilum/windows/builds/7000/Setup.exe'] = b'MZcorrupt'
            with self.assertRaises(ValueError): p.promote(7000, store)
            self.assertNotIn('okilum/windows/stable/releases.stable.json', store.data)


if __name__ == '__main__': unittest.main()
