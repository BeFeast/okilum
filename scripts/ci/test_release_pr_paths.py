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

    def test_packaging_and_release_logic_still_run(self):
        for workflow, platform in [
            ('linux-release.yml', 'scripts/arch/PKGBUILD'),
            ('windows-diagnostic.yml', 'scripts/windows/pack.sh'),
        ]:
            paths = patterns(workflow)
            for path in [platform, 'packaging/icons/icon.png', 'scripts/releases/coalesce.py',
                         '.forgejo/workflows/' + workflow]:
                self.assertTrue(any(fnmatch.fnmatchcase(path, p) for p in paths), path)

    def test_shared_dependencies_require_explicit_branch_dispatch(self):
        for workflow in ['linux-release.yml', 'windows-diagnostic.yml']:
            paths = patterns(workflow)
            for path in ['Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml',
                         'crates/tessera-shell/Cargo.toml', 'crates/tessera-shell/build.rs',
                         'scripts/vendor-setup.sh', 'scripts/patches/0001-test.diff',
                         'scripts/ci/release-cache.sh']:
                self.assertFalse(any(fnmatch.fnmatchcase(path, p) for p in paths), path)
            text = (ROOT / '.forgejo/workflows' / workflow).read_text()
            self.assertIn('  workflow_dispatch:', text)

    def test_main_and_scheduled_delivery_remain_enabled(self):
        linux = (ROOT / '.forgejo/workflows/linux-release.yml').read_text()
        windows = (ROOT / '.forgejo/workflows/windows-diagnostic.yml').read_text()
        self.assertIn('  push:\n    branches: [main]', linux)
        self.assertIn('  schedule:', windows)

    def test_windows_cross_build_helpers_still_trigger(self):
        paths = patterns('windows-diagnostic.yml')
        for path in ['scripts/build-windows-ci.sh', 'scripts/windows-icons.py']:
            self.assertTrue(any(fnmatch.fnmatchcase(path, p) for p in paths), path)
