"""Explicit-source promotion (#1040): build what is missing, promote when complete."""
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'updater'))
import catalog
import prepare

SOURCE = 'a' * 40


class Store:
    def __init__(self):
        self.data = {}

    def call(self, method, key):
        assert method == 'GET'
        return self.data.get(key)

    def put(self, key, data, *_):
        self.data[key] = data


def built(store, *platforms):
    for platform in platforms:
        store.data[f'{catalog.PREFIX}/{SOURCE}/{platform}.json'] = catalog.encode(
            {'platform': platform, 'build': {'macos': 10318, 'linux': 10338, 'windows': 10500}[platform],
             'source': SOURCE, 'assets': []})


def dispatched(client):
    return [(call.args[1], call.args[2]['inputs']) for call in client.call.call_args_list]


@patch.object(prepare.subprocess, 'run', return_value=Mock(returncode=0))
class Request(unittest.TestCase):
    def test_missing_builds_are_dispatched_with_the_source_and_recorded(self, _):
        store, client = Store(), Mock()
        built(store, 'macos', 'linux')
        self.assertEqual(prepare.request(store, client, SOURCE), ['windows'])
        self.assertEqual(dispatched(client), [('/actions/workflows/windows-diagnostic.yml/dispatches',
                                              {'source': SOURCE})])
        pending = json.loads(store.data[f'{prepare.REQUESTS}/{SOURCE}.json'])
        self.assertEqual((pending['state'], pending['macos'], pending['missing']), ('waiting', 10318, ['windows']))

    def test_a_complete_triple_is_promoted_at_once(self, _):
        store, client = Store(), Mock()
        built(store, 'macos', 'linux', 'windows')
        self.assertEqual(prepare.request(store, client, SOURCE), [])
        self.assertEqual(dispatched(client), [('/actions/workflows/releases.yml/dispatches', {'build': '10318'})])
        self.assertNotIn(f'{prepare.REQUESTS}/{SOURCE}.json', store.data)

    def test_no_macos_build_is_refused_before_anything_starts(self, _):
        store, client = Store(), Mock()
        built(store, 'linux')
        with self.assertRaisesRegex(ValueError, 'No macOS build'):
            prepare.request(store, client, SOURCE)
        client.call.assert_not_called()
        self.assertEqual(store.data.keys() - {f'{catalog.PREFIX}/{SOURCE}/linux.json'}, set())

    def test_short_sha_and_commits_off_main_are_refused(self, run):
        store, client = Store(), Mock()
        built(store, 'macos')
        with self.assertRaisesRegex(ValueError, 'full 40-character'):
            prepare.request(store, client, 'abc123')
        run.return_value = Mock(returncode=1)
        with self.assertRaisesRegex(ValueError, 'not a commit on main'):
            prepare.request(store, client, SOURCE)
        client.call.assert_not_called()


class Complete(unittest.TestCase):
    def test_promotes_once_when_the_last_build_lands(self):
        store, client = Store(), Mock()
        built(store, 'macos', 'linux')
        store.data[f'{prepare.REQUESTS}/{SOURCE}.json'] = catalog.encode(
            {'source': SOURCE, 'macos': 10318, 'state': 'waiting', 'missing': ['windows']})
        # Still missing Windows: nothing happens.
        self.assertFalse(prepare.complete(store, client, SOURCE))
        client.call.assert_not_called()
        built(store, 'windows')
        self.assertTrue(prepare.complete(store, client, SOURCE))
        self.assertEqual(dispatched(client), [('/actions/workflows/releases.yml/dispatches', {'build': '10318'})])
        # A retried publication of the same build does not promote again.
        self.assertFalse(prepare.complete(store, client, SOURCE))
        self.assertEqual(client.call.call_count, 1)

    def test_publications_without_a_request_do_nothing(self):
        store, client = Store(), Mock()
        built(store, 'macos', 'linux', 'windows')
        self.assertFalse(prepare.complete(store, client, SOURCE))
        client.call.assert_not_called()


if __name__ == '__main__':
    unittest.main()
