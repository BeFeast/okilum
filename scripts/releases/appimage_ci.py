"""Hosted-only signing/install regression for the actual AppImage; no live publisher."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

import appimage_publish as publisher


class LocalStore:
    def __init__(self, root):
        self.root = root

    def call(self, method, key):
        assert method == 'GET'
        path = self.root / key
        return path.read_bytes() if path.exists() else None

    def put(self, key, data, *_):
        path = self.root / key
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)


def main(directory):
    provenance = json.loads((directory / 'provenance.json').read_bytes())
    source = provenance['source_sha']
    receipt = {'run': {'id': int(provenance['workflow_run']),
                       'run_number': int(provenance['build_version']) - 5000,
                       'run_attempt': int(provenance['workflow_attempt'])},
               'artifact': {'id': 1, 'digest': 'sha256:' + '0' * 64}}
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        home = root / 'key'; home.mkdir(mode=0o700)
        subprocess.run(['gpg', '--homedir', str(home), '--batch', '--passphrase', '',
                        '--quick-generate-key', 'AppImage CI <ci@example.invalid>', 'ed25519', 'sign', '1d'], check=True)
        try:
            info = publisher.arch.run('gpg', '--homedir', str(home), '--with-colons', '--list-secret-keys').decode()
            fingerprint = next(line.split(':')[9] for line in info.splitlines() if line.startswith('fpr:'))
            secret = publisher.arch.run('gpg', '--homedir', str(home), '--armor', '--export-secret-keys').decode()
            store = LocalStore(root / 'store')
            work = root / 'work'; work.mkdir()
            # The test key substitution is confined to this hosted regression, never a CLI option.
            with patch.dict(os.environ, ARCH_GPG_PRIVATE_KEY=secret, ARCH_GPG_FINGERPRINT=fingerprint), \
                    patch.object(publisher, 'FINGERPRINT', fingerprint), publisher.arch.SigningKey() as key:
                descriptor = publisher.publish(store, key, work, directory, source, receipt, 9001)
                publisher.publish(store, key, work, directory, source, receipt, 9002)
                installed = root / 'install'; installed.mkdir()
                for name in [publisher.artifact.IMAGE, publisher.artifact.IMAGE + '.sig', 'latest.json', 'latest.json.sig']:
                    (installed / name).write_bytes(store.call('GET', f'okilum/linux/beta/{name}'))
                key.verify(installed / 'latest.json')
                key.verify(installed / publisher.artifact.IMAGE)
                if publisher.digest((installed / publisher.artifact.IMAGE).read_bytes()) != descriptor['sha256']:
                    raise ValueError('Installed payload hash mismatch')
                image = installed / publisher.artifact.IMAGE
                image.chmod(0o755)
                # FUSE-free launch uses the installed, verified bytes.
                subprocess.run(['bash', 'scripts/appimage/launch.sh', str(image), '--help'], check=True)
                image.write_bytes(image.read_bytes() + b'tampered')
                try:
                    key.verify(image)
                except subprocess.CalledProcessError:
                    pass
                else:
                    raise ValueError('Tampered installed AppImage was accepted')
            print('APPIMAGE_SIGNED_INSTALL_PASS: disposable key, immutable readback, retry, download, signature, help, tamper rejection')
        finally:
            subprocess.run(['gpgconf', '--homedir', str(home), '--kill', 'all'], check=True)


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve())
