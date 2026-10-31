import base64
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile
from macos_hosted_handoff import stage
from sparkle_sign import sign_archive


class HostedBeta(unittest.TestCase):
    def payload(self, alter=None):
        assets = [{'name': n, 'size': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}
                  for n, raw in [('app.zip', b'signed archive'), ('app.dmg', b'signed dmg')]]
        provenance = {'source': 'a' * 40, 'workflow_sha': 'b' * 40, 'tree': 'c' * 40,
                      'build': 5500, 'producer': '700', 'producer_attempt': '1', 'github_run': '900', 'assets': assets}
        metadata = {'SOURCE_SHA': 'a' * 40, 'SOURCE_TREE': 'c' * 40, 'BUILD': '5500',
                    'DISPLAY_VERSION': '0.1.5500', 'archive': 'app.zip', 'dmg': 'app.dmg'}
        files = {'app.zip': b'signed archive', 'app.dmg': b'signed dmg',
                 'provenance.json': json.dumps(provenance).encode(), 'release.json': json.dumps(metadata).encode()}
        if alter:
            alter(files)
        out = io.BytesIO()
        with zipfile.ZipFile(out, 'w') as archive:
            for name, data in files.items():
                archive.writestr(name, data)
        data = out.getvalue()
        return data, {'digest': 'sha256:' + hashlib.sha256(data).hexdigest()}

    def test_only_matching_source_producer_payload_is_staged(self):
        with tempfile.TemporaryDirectory() as temp:
            data, item = self.payload()
            root = Path(temp) / 'valid'
            self.assertEqual(stage(data, item, 'a' * 40, 'b' * 40, 5500, 700, 900, root)['BUILD'], '5500')
            self.assertEqual((root / 'app.zip').read_bytes(), b'signed archive')
            for i, change in enumerate([lambda f: f.update({'app.zip': b'changed'}),
                                      lambda f: f.update({'../escape': b'bad'}),
                                      lambda f: f.update({'release.json': b'{}'})]):
                data, item = self.payload(change)
                with self.assertRaises((ValueError, KeyError)):
                    stage(data, item, 'a' * 40, 'b' * 40, 5500, 700, 900, Path(temp) / str(i))
            data, item = self.payload()
            for source, workflow, build, producer, github in [
                    ('d' * 40, 'b' * 40, 5500, 700, 900), ('a' * 40, 'd' * 40, 5500, 700, 900),
                    ('a' * 40, 'b' * 40, 5501, 700, 900), ('a' * 40, 'b' * 40, 5500, 701, 900),
                    ('a' * 40, 'b' * 40, 5500, 700, 901)]:
                with self.assertRaises(ValueError):
                    stage(data, item, source, workflow, build, producer, github, Path(temp) / 'bad')
            with self.assertRaises(ValueError):
                stage(data + b'tampered', item, 'a' * 40, 'b' * 40, 5500, 700, 900, Path(temp) / 'bad')

    def test_standard_sparkle_seed_signs_with_pinned_public_and_rejects_other_keys(self):
        # RFC8032 test key, never a production credential.
        seed = bytes.fromhex('9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60')
        public = bytes.fromhex('d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a')
        with tempfile.TemporaryDirectory() as temp:
            archive = Path(temp) / 'update.zip'; archive.write_bytes(b'fixture archive')
            encoded = base64.b64encode(seed).decode()
            signature = sign_archive(archive, encoded, base64.b64encode(public).decode())
            self.assertEqual(len(base64.b64decode(signature)), 64)
            with self.assertRaises(ValueError):
                sign_archive(archive, encoded, base64.b64encode(bytes(32)).decode())
            with self.assertRaises(ValueError):
                sign_archive(archive, base64.b64encode(bytes(96)).decode())


if __name__ == '__main__':
    unittest.main()
