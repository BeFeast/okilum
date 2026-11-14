"""Image inputs fail closed and the Docker context never inherits the checkout."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

RECIPE = Path(__file__).with_name("reader-image")
spec = importlib.util.spec_from_file_location("reader_image", RECIPE / "build.py")
image = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image)


class ReaderImageTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[2] / "target/ci-reader-image-tests"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.pin = self.root / "rust-toolchain.toml"
        self.pin.write_text('[toolchain]\nchannel="1.100.0"\nprofile="minimal"\ncomponents=["clippy","rustfmt"]\n')

    def test_context_allowlist_excludes_secret_and_source(self):
        (self.root / ".env").write_text("secret=must-not-enter-image")
        (self.root / "main.rs").write_text("private source")
        context = self.root / "context"
        context.mkdir()
        version, base = image.prepare_context(self.root, RECIPE, context)
        self.assertEqual(version, "1.100.0")
        self.assertIn("@sha256:", base)
        self.assertEqual({p.name for p in context.iterdir()},
                         {"Dockerfile", "packages.txt", "rust-toolchain.toml"})
        self.assertEqual((context / "rust-toolchain.toml").read_bytes(), self.pin.read_bytes())

    def test_version_comes_from_toolchain_not_recipe(self):
        self.pin.write_text(self.pin.read_text().replace("1.100.0", "1.100.2"))
        self.assertEqual(image.inputs(self.root, RECIPE)[0], "1.100.2")

    def test_floating_missing_and_changed_components_are_rejected(self):
        original = self.pin.read_text()
        for text in (original.replace("1.100.0", "stable"),
                     original.replace("1.100.0", "1.100"),
                     original.replace('"clippy",', ''),
                     original + 'targets=["aarch64-unknown-linux-gnu"]\n'):
            with self.subTest(text=text):
                self.pin.write_text(text)
                with self.assertRaises(ValueError):
                    image.inputs(self.root, RECIPE)
        self.pin.unlink()
        with self.assertRaises(FileNotFoundError):
            image.inputs(self.root, RECIPE)

    def test_failed_preflight_removes_old_success_receipt(self):
        artifact = self.root / "output"
        artifact.mkdir()
        receipt = artifact / "receipt.json"
        receipt.write_text('{"published":true,"digest":"old-success"}')
        self.pin.unlink()
        with mock.patch.object(image, "ROOT", self.root), \
             mock.patch("sys.argv", ["build.py", "--output", str(artifact)]), \
             mock.patch.object(image, "run") as docker:
            with self.assertRaises(FileNotFoundError):
                image.main()
            docker.assert_not_called()
        self.assertFalse(receipt.exists())

if __name__ == "__main__":
    unittest.main()
