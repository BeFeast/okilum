"""Mac-only input routing, provenance and cross-platform write isolation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import catalog
import publish as p
from test_publish import Store, fixture, SOURCE


def prepared():
    store = Store()
    release = fixture(store)
    mac = release['platforms']['macos']['assets'][0]
    tree = p.ET.fromstring(store.data['okilum/appcast.xml'])
    item = tree.find('./channel/item')
    p.ET.SubElement(item, 'enclosure', {
        'url': 'https://updates.befeast.com/' + mac['key'],
        'length': str(mac['size']), p.appcast.s('edSignature'): 'signed-mac',
    })
    p.ET.SubElement(item, 'description').text = 'Existing release notes'
    store.data['okilum/appcast.xml'] = p.ET.tostring(tree)
    # There need not be any completed Linux/Windows artifact for this source.
    for platform in ('linux', 'windows'):
        del store.data[f'{catalog.PREFIX}/{SOURCE}/{platform}.json']
        store.data[f'okilum/{platform}/stable/untouched'] = b'previous feed'
    store.writes.clear()
    return store


class MacOnlyTests(unittest.TestCase):
    def test_only_mac_writes_no_tag_notes_or_other_platform_dependency(self):
        store = prepared()
        before = dict(store.data)
        with patch.object(p, 'mirror_tag') as tag, patch.object(p, 'github_release') as github, \
             patch.object(p.release_notes, 'record') as notes, patch.object(p.arch, 'execute') as arch, \
             patch.object(p.windows, 'promote') as windows:
            p.promote_mac_only(store, 700)
        for mock in (tag, github, notes, arch, windows):
            mock.assert_not_called()
        allowed = {'okilum/appcast.xml', 'okilum/macos/latest.zip', 'okilum/macos/stable/Okilum.dmg',
                   f'{catalog.PREFIX}/macos-promoting.json', f'{catalog.PREFIX}/macos-stable.json'}
        self.assertEqual(set(store.writes), allowed)
        for key, value in before.items():
            if key not in allowed:
                self.assertEqual(store.data[key], value, key)
        item = p.ET.fromstring(store.data['okilum/appcast.xml']).find('./channel/item')
        self.assertEqual(item.findtext(p.appcast.s('channel')), 'stable')
        self.assertEqual(item.findtext('description'), 'Existing release notes')
        self.assertEqual(item.find('enclosure').get(p.appcast.s('edSignature')), 'signed-mac')

    def test_corrupt_payload_or_wrong_provenance_fails_before_writes(self):
        for defect in ('checksum', 'source', 'signature', 'rollback'):
            with self.subTest(defect=defect):
                store = prepared()
                if defect == 'checksum':
                    store.data['archive/Okilum-0.1.700.dmg'] += b'corrupt'
                elif defect == 'source':
                    key = f'{catalog.PREFIX}/{SOURCE}/macos.json'
                    value = json.loads(store.data[key]); value['source'] = 'b' * 40
                    store.data[key] = catalog.encode(value)
                else:
                    tree = p.ET.fromstring(store.data['okilum/appcast.xml'])
                    if defect == 'signature':
                        tree.find('./channel/item/enclosure').attrib.pop(p.appcast.s('edSignature'))
                    else:
                        item = p.ET.SubElement(tree.find('channel'), 'item')
                        p.ET.SubElement(item, p.appcast.s('version')).text = '701'
                        p.ET.SubElement(item, p.appcast.s('channel')).text = 'stable'
                    store.data['okilum/appcast.xml'] = p.ET.tostring(tree)
                with self.assertRaises(ValueError):
                    p.promote_mac_only(store, 700)
                self.assertEqual(store.writes, [])

    def test_interrupted_promotion_needs_explicit_newer_supersede(self):
        for scope in ('', 'macos-'):
            store = prepared()
            store.data[f'{catalog.PREFIX}/{scope}promoting.json'] = catalog.encode({'build': 699})
            with self.assertRaisesRegex(ValueError, 'interrupted'):
                p.promote_mac_only(store, 700)
            self.assertEqual(store.writes, [])
            p.promote_mac_only(store, 700, supersede_pending=True)
            self.assertIn(f'{catalog.PREFIX}/macos-stable.json', store.writes)

    def test_input_routes_before_legacy_dispatch_and_rejects_ambiguous_modes(self):
        text = (Path(__file__).resolve().parents[2] / '.forgejo/workflows/releases.yml').read_text()
        self.assertIn('MAC_ONLY: ${{ inputs.mac_only }}', text)
        script = '\n'.join(line[10:] for line in text.split('        run: |\n', 1)[1].splitlines())
        with tempfile.TemporaryDirectory() as directory:
            stub = Path(directory) / 'python'
            stub.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n'); stub.chmod(0o755)
            env = {**os.environ, 'PATH': directory + os.pathsep + os.environ['PATH'],
                   'MAC_ONLY': 'true', 'PROMOTE_BUILD': '700', 'PROMOTE_SOURCE': '',
                   'LINUX_ONLY': 'false', 'LINUX_ROLLBACK': 'false', 'WINDOWS_ONLY': 'false',
                   'SKIP_WINDOWS': 'false', 'SUPERSEDE_PENDING': 'true'}
            result = subprocess.run(['bash', '-c', script], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.splitlines(), ['scripts/releases/publish.py', '--build', '700',
                                                         '--mac-only', '--supersede-pending'])
            for key, value in [('PROMOTE_BUILD', ''), ('PROMOTE_SOURCE', SOURCE), ('LINUX_ONLY', 'true'),
                               ('LINUX_ROLLBACK', 'true'), ('WINDOWS_ONLY', 'true'), ('SKIP_WINDOWS', 'true')]:
                result = subprocess.run(['bash', '-c', script], env={**env, key: value}, capture_output=True)
                self.assertNotEqual(result.returncode, 0, key)
                self.assertEqual(result.stdout, b'', key)
