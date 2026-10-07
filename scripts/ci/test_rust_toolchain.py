"""Guard against CI/release scripts overriding the repository Rust pin."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ToolchainTests(unittest.TestCase):
    def test_explicit_toolchain_and_components(self):
        config = (ROOT / "rust-toolchain.toml").read_text()
        self.assertRegex(config, r'channel = "[0-9]+\.[0-9]+\.[0-9]+"')
        self.assertIn('"clippy"', config)
        self.assertIn('"rustfmt"', config)

    def test_reader_builds_do_not_override_pin(self):
        paths = list((ROOT / ".forgejo/workflows").glob("*.yml"))
        paths += list((ROOT / ".github/workflows").glob("*.yml"))
        paths += list((ROOT / "scripts").glob("build-*.sh"))
        paths += [ROOT / "scripts/ci/check-macos.sh", ROOT / "scripts/arch/build.sh",
                  ROOT / "scripts/arch/PKGBUILD"]
        for path in paths:
            with self.subTest(path=path.name):
                self.assertIsNone(re.search(r"cargo \+(?:[0-9]|stable)|rustup (?:default|toolchain install) (?:stable|[0-9])", path.read_text()))

    def test_hosted_cache_tracks_toolchain(self):
        workflow = (ROOT / ".github/workflows/forgejo-macos.yml").read_text()
        self.assertIn("hashFiles('rust-toolchain.toml',", workflow)
