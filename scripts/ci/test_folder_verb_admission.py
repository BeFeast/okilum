"""Original-package identity and non-canonical execution refusal for #1914."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).parents[1] / 'windows/qa-harness/folder-verb'
SPEC = importlib.util.spec_from_file_location('folder_verb', ROOT / 'run.py')
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class FolderVerbAdmission(unittest.TestCase):
    def fixture(self, root):
        source = 'a' * 40
        package = io.BytesIO()
        with zipfile.ZipFile(package, 'w') as archive:
            archive.writestr('BeFeast.Okilum.nuspec', '<package><metadata><id>BeFeast.Okilum</id><version>0.1.5226</version></metadata></package>')
            archive.writestr('lib/app/okilum.exe', b'exact compiled PR executable')
        artifact = root / 'original.zip'
        with zipfile.ZipFile(artifact, 'w') as archive:
            archive.writestr('BeFeast.Okilum-0.1.5226-beta-full.nupkg', package.getvalue())
            archive.writestr('BeFeast.Okilum-beta-Setup.exe', b'original Setup')
        setup = root / 'Setup.exe'; setup.write_bytes(b'original Setup')
        metadata = root / 'artifact.json'
        metadata.write_text(json.dumps({'workflow_run': {'head_sha': source, 'id': 42}, 'expired': False, 'name': 'unsigned-windows', 'digest': 'sha256:' + hashlib.sha256(artifact.read_bytes()).hexdigest()}))
        run = root / 'run.json'
        run.write_text(json.dumps({'head_sha': source, 'id': 42, 'status': 'completed', 'conclusion': 'success', 'path': '.github/workflows/forgejo-windows-release.yml'}))
        manifest = root / 'manifest.json'
        manifest.write_text(json.dumps({'source': source, 'artifact_zip': str(artifact), 'setup': str(setup), 'artifact_metadata': str(metadata), 'hosted_run': str(run)}))
        return manifest, setup, metadata

    def test_installed_executable_hash_comes_from_original_package(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest, _, _ = self.fixture(Path(directory))
            admitted = runner.admit(manifest)
            self.assertEqual(admitted['source'], 'a' * 40)
            self.assertEqual(admitted['version'], '0.1.5226')
            self.assertEqual(admitted['exe_sha256'], hashlib.sha256(b'exact compiled PR executable').hexdigest())

    def test_replacement_setup_and_wrong_head_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest, setup, metadata = self.fixture(Path(directory))
            setup.write_bytes(b'substituted bytes')
            with self.assertRaisesRegex(ValueError, 'Setup differs'):
                runner.admit(manifest)
            setup.write_bytes(b'original Setup')
            data = json.loads(metadata.read_text()); data['workflow_run']['head_sha'] = 'b' * 40
            metadata.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, 'source/name/expiry'):
                runner.admit(manifest)

    def test_product_checkout_cannot_start_native_execution(self):
        with self.assertRaisesRegex(RuntimeError, 'canonical harness'):
            runner.execute('unused', 'unused', 'not an execution grant')


if __name__ == '__main__':
    unittest.main()
