import pathlib
import json
import os
from unittest.mock import patch
import tempfile
import unittest
import xml.etree.ElementTree as ET

import appcast

SOURCE = 'a' * 40


def add_args(build, channel='beta'):
    return ['add', '--build', str(build), '--short-version', f'0.1.{build}',
            '--channel', channel, '--length', '10', '--signature', 'c2ln',
            '--source', SOURCE, '--tree', 'b' * 40, '--url',
            f'https://git.oklabs.uk/BeFeast/okilum/releases/download/macos-stable-{build}/'
            f'okilum-macos-arm64-{SOURCE}-notarized.zip']


class AppcastTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = str(pathlib.Path(self.dir.name) / 'appcast.xml')

    def tearDown(self):
        self.dir.cleanup()

    def run_tool(self, *args, existing=True):
        prefix = ['--appcast', self.path] if existing else []
        appcast.main(prefix + ['--output', self.path] + list(args))
        return ET.parse(self.path)

    def channels(self, tree):
        return {i.findtext(appcast.s('version')): i.findtext(appcast.s('channel'))
                for i in appcast.items(tree)}

    def test_add_then_promote(self):
        self.run_tool(*add_args(5001), existing=False)
        tree = self.run_tool(*add_args(5002))
        self.assertEqual(self.channels(tree), {'5001': 'beta', '5002': 'beta'})
        newest = appcast.items(tree)[0]
        self.assertEqual(newest.findtext(appcast.s('version')), '5002')
        self.assertEqual(newest.findtext(appcast.t('source')), SOURCE)
        enclosure = newest.find('enclosure')
        self.assertEqual(enclosure.get(appcast.s('edSignature')), 'c2ln')
        tree = self.run_tool('promote', '--build', '5001')
        self.assertEqual(self.channels(tree), {'5001': 'stable', '5002': 'beta'})

    def test_notes_description_and_promotion_preserve_same_items(self):
        notes_path = pathlib.Path(self.dir.name) / 'notes.json'
        payload = {'build': 5001, 'source': SOURCE, 'platform': 'macos', 'channel': 'beta',
                   'previous_build': 5000, 'previous_source': 'c' * 40,
                   'items': [{'pr': 1482, 'kind': 'feature', 'text': 'Outline <code> & text'}]}
        notes_path.write_text(json.dumps(payload))
        with patch.dict(os.environ, OKILUM_RELEASE_NOTES=str(notes_path)):
            tree = self.run_tool(*add_args(5001), existing=False)
        item = appcast.items(tree)[0]
        text = item.findtext('description')
        self.assertIn('Outline &lt;code&gt; &amp; text', text)
        self.assertIn('/pulls/1482', text)
        self.assertEqual(item.find('enclosure').get(appcast.s('edSignature')), 'c2ln')
        promoted = self.run_tool('promote', '--build', '5001')
        self.assertEqual(appcast.items(promoted)[0].findtext('description'), text)

    def test_notes_for_different_build_source_platform_or_channel_are_rejected(self):
        notes_path = pathlib.Path(self.dir.name) / 'notes.json'
        original = {'build': 5001, 'source': SOURCE, 'platform': 'macos', 'channel': 'beta', 'items': []}
        for changes in [{'build': 5002}, {'source': 'b' * 40}, {'platform': 'windows'}, {'channel': 'stable'}]:
            notes_path.write_text(json.dumps({**original, **changes}))
            with patch.dict(os.environ, OKILUM_RELEASE_NOTES=str(notes_path)), self.assertRaises(ValueError):
                self.run_tool(*add_args(5001), existing=False)

    def test_refuses_replay_and_unknown_promotion(self):
        self.run_tool(*add_args(5002), existing=False)
        with self.assertRaises(SystemExit):
            self.run_tool(*add_args(5002))
        with self.assertRaises(SystemExit):
            self.run_tool(*add_args(5001))
        with self.assertRaises(SystemExit):
            self.run_tool('promote', '--build', '4000')


if __name__ == '__main__':
    unittest.main()
