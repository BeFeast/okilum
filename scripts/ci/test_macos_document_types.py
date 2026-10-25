"""Check the actual shipping template and Sprint's plist transformation."""
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class DocumentTypesTests(unittest.TestCase):
    def template(self):
        source = (ROOT / 'scripts/build-macos-ci.sh').read_text()
        return source.split('cat >"$APP/Contents/Info.plist" <<PLIST\n', 1)[1].split('\nPLIST\n', 1)[0]

    def assert_drop_types(self, info, rank):
        documents = info['CFBundleDocumentTypes']
        folders = [d for d in documents if 'public.folder' in d['LSItemContentTypes']]
        markdown = [d for d in documents if 'net.daringfireball.markdown' in d['LSItemContentTypes']]
        self.assertEqual(len(folders), 1)
        self.assertEqual(len(markdown), 1)
        for document in folders + markdown:
            self.assertEqual(document['CFBundleTypeRole'], 'Viewer')
            self.assertEqual(document['LSHandlerRank'], rank)
        # Markdown conforms to plain text without claiming every plain-text file.
        declaration = next(d for d in info['UTImportedTypeDeclarations']
                           if d['UTTypeIdentifier'] == 'net.daringfireball.markdown')
        self.assertIn('public.plain-text', declaration['UTTypeConformsTo'])
        self.assertEqual(declaration['UTTypeTagSpecification']['public.filename-extension'], ['md'])

    def test_shipping_accepts_folders_and_markdown(self):
        self.assert_drop_types(plistlib.loads(self.template().encode()), 'Alternate')

    def test_sprint_accepts_explicit_drops_without_default_or_url_associations(self):
        source = (ROOT / 'scripts/dev/okilum-sprint.sh').read_text()
        program = source.split('python3 - "$bundle_stage" "$sha" <<\'PY\'\n', 1)[1].split('\nPY\n', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            stage = Path(directory)
            (stage / 'Contents').mkdir()
            subprocess.run(['python3', '-', str(stage), 'test-source'], input=program,
                           text=True, cwd=ROOT, check=True)
            info = plistlib.loads((stage / 'Contents/Info.plist').read_bytes())
        self.assert_drop_types(info, 'None')
        self.assertNotIn('CFBundleURLTypes', info)
        self.assertEqual(info['CFBundleIdentifier'], 'com.befeast.okilum.sprint')


if __name__ == '__main__':
    unittest.main()
