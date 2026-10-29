import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile
from copied_artifact import admit, members, sha, stage


def archive(entries):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as z:
        for name, data in entries: z.writestr(name, data)
    return stream.getvalue()


class Admission(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.source = 'a' * 40
        self.payload = [('.portable', b''), ('Okilum.exe', b'MZlauncher'), ('Update.exe', b'MZupdate'), ('current/okilum.exe', b'MZtest'), ('current/sq.version', b'<package><metadata><version>0.1.5226</version></metadata></package>'), ('current/gpui-shaders/a', b'shader')]
        self.run = dict(head_sha=self.source, id=1, status='completed', conclusion='success',
                        path='.github/workflows/forgejo-windows-release.yml')
        self.meta = dict(workflow_run=dict(head_sha=self.source, id=1), expired=False, name='unsigned-windows')
        self.prepare()

    def prepare(self):
        blob = archive([('BeFeast.Okilum-beta-Portable.zip', archive(self.payload))])
        (self.root/'artifact.zip').write_bytes(blob)
        self.meta['digest'] = 'sha256:' + sha(blob)
        for name, value in [('run', self.run), ('meta', self.meta)]:
            (self.root/name).write_text(json.dumps(value))
        self.manifest = self.root/'manifest.json'
        self.manifest.write_text(json.dumps(dict(source=self.source, hosted_run=str(self.root/'run'),
            artifact_metadata=str(self.root/'meta'), artifact_zip=str(self.root/'artifact.zip'))))

    def test_original_bundle_and_source(self):
        result = admit(self.manifest)
        self.assertEqual(result['version'], '0.1.5226')
        self.assertEqual(result['files']['current/okilum.exe'], sha(b'MZtest'))
        stage(self.manifest, self.root/'stage')
        with self.assertRaises(FileExistsError): stage(self.manifest, self.root/'stage')

    def test_failed_or_wrong_head_run_refused(self):
        for key, bad in [('head_sha', 'b'*40), ('conclusion', 'failure'), ('status', 'queued'), ('path', 'other.yml')]:
            old = self.run[key]; self.run[key] = bad; self.prepare()
            with self.assertRaises(ValueError): admit(self.manifest)
            self.run[key] = old

    def test_wrong_artifact_provenance(self):
        self.meta['workflow_run']['id'] = 2; self.prepare()
        with self.assertRaises(ValueError): admit(self.manifest)

    def test_changed_original_bytes(self):
        (self.root/'artifact.zip').write_bytes(b'changed')
        with self.assertRaises(ValueError): admit(self.manifest)

    def test_portable_marker_and_version_required(self):
        for missing in ['.portable', 'current/sq.version']:
            previous = self.payload[:]
            self.payload = [(n, b) for n, b in previous if n != missing]; self.prepare()
            with self.assertRaises(ValueError): admit(self.manifest)
            self.payload = previous

    def test_missing_shader_or_executable_refused(self):
        for payload in [[('okilum.exe', b'MZ')], [('gpui-shaders/a', b'x')]]:
            self.payload = payload; self.prepare()
            with self.assertRaises(ValueError): admit(self.manifest)

    def test_windows_collisions_refused(self):
        for entries in [[('A',b'x'),('a',b'y')],[('a',b'x'),('a/b',b'y')]]:
            with self.assertRaises(ValueError): members(archive(entries))

    def test_path_escape_refused(self):
        for name in ['../bad', '/bad', 'C:/bad', 'a\\b', 'a/../b', 'CON', 'x/NUL.txt', 'name.', 'name ']:
            with self.assertRaises(ValueError): members(archive([(name, b'x')]))


if __name__ == '__main__': unittest.main()
