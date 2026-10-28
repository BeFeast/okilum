#!/usr/bin/env python3
"""Verify the actual legal files staged into release bundles."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('notices', ROOT / 'scripts/third-party-notices.py')
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class BundleNotices(unittest.TestCase):
    def test_generator_includes_mermaid_font_notice(self):
        import json
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'licenses').mkdir()
            (root / 'licenses/README.md').write_text('Sources')
            fonts = root / 'crates/okilum-shell/assets/mermaid'
            fonts.mkdir(parents=True)
            (fonts / 'LICENSE-Arimo').write_text('Arimo Apache-2.0 control')
            report = root / 'about.json'
            report.write_text(json.dumps({'licenses': []}))
            with patch.object(notices, 'ROOT', root):
                notices.generate(report)
            generated = (root / 'THIRD_PARTY_NOTICES.md').read_text()
            self.assertIn('assets/mermaid/LICENSE-Arimo', generated)
            self.assertIn('Arimo Apache-2.0 control', generated)

    def test_staging_includes_complete_committed_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'Resources' / 'Licenses'
            notices.stage(destination)
            for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md'):
                self.assertEqual((destination / name).read_bytes(), (ROOT / name).read_bytes())
            text = (destination / 'THIRD_PARTY_NOTICES.md').read_text()
            for marker in ('gpui-component', 'MPL-2.0', 'Unicode-3.0', 'Excalifont',
                           'SIL OPEN FONT LICENSE', 'Cole Bemis', 'Sparkle',
                           'merman-elk-layered 0.9.2', 'EPL-2.0',
                           'assets/mermaid/LICENSE-Arimo'):
                self.assertIn(marker, text)
            # Staging again must preserve the actual contents, not stale output.
            (destination / 'LICENSE').write_text('stale')
            notices.stage(destination)
            self.assertEqual((destination / 'LICENSE').read_bytes(), (ROOT / 'LICENSE').read_bytes())


if __name__ == '__main__':
    unittest.main()
