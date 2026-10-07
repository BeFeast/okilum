"""Ordinary Reader PRs must not occupy native packaging runners."""
import fnmatch
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def patterns(workflow):
    text = (ROOT / '.forgejo/workflows' / workflow).read_text()
    section = text.split('  pull_request:\n', 1)[1]
    section = re.split(r'\n  [^ ]', section, maxsplit=1)[0]
    return re.findall(r"      - '([^']+)'", section)


class ReleasePathsTests(unittest.TestCase):
    def test_ui_docs_and_other_platform_do_not_build_packages(self):
        for workflow, unrelated in [
            ('linux-release.yml', 'scripts/windows/pack.sh'),
            ('windows-diagnostic.yml', 'scripts/arch/PKGBUILD'),
        ]:
            paths = patterns(workflow)
            self.assertTrue(paths)
            for path in ['crates/tessera-shell/src/reader_settings.rs',
                         'docs/releases.md', 'README.md', unrelated]:
                self.assertFalse(any(fnmatch.fnmatchcase(path, p) for p in paths), path)

    def test_packaging_and_shared_build_inputs_still_run(self):
        for workflow, platform in [
            ('linux-release.yml', 'scripts/arch/PKGBUILD'),
            ('windows-diagnostic.yml', 'scripts/windows/pack.sh'),
        ]:
            paths = patterns(workflow)
            for path in [platform, 'rust-toolchain.toml', 'Cargo.lock',
                         'scripts/ci/release-cache.sh', 'scripts/third-party-notices.py',
                         'crates/tessera-shell/build.rs', '.forgejo/workflows/' + workflow]:
                self.assertTrue(any(fnmatch.fnmatchcase(path, p) for p in paths), path)
