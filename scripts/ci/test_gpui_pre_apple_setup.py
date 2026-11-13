"""Offline controls for the checksum-pinned GPUI macOS first-window patches (#2825)."""
import difflib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("apple_setup", ROOT / "scripts/gpui-pre-apple-setup.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)

FILES = {
    "gpui-pre-macos": {"Cargo.toml": b"macos\n", "src/window.rs": b"window\n", "src/platform.rs": b"platform\n"},
    "gpui-pre-apple": {"Cargo.toml": b"apple\n", "src/metal_renderer.rs": b"renderer\n"},
}
PATCHES = {
    "gpui-pre-macos": [("0001.diff", "src/window.rs"), ("0002.diff", "src/platform.rs")],
    "gpui-pre-apple": [("0001.diff", "src/metal_renderer.rs")],
}


def diff(name, before, after):
    return "".join(difflib.unified_diff([before], [after], fromfile="a/" + name, tofile="b/" + name))


class GpuiAppleSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gpui-apple-vendor-control-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/gpui-pre-apple-setup.py", self.root / "scripts/gpui-pre-apple-setup.py")
        self.archives = self.root / "archives"
        self.archives.mkdir()
        self.manifests = {}
        for package, files in FILES.items():
            with tarfile.open(self.archives / f"{package}-0.3.3.crate", "w:gz") as archive:
                for name, data in files.items():
                    info = tarfile.TarInfo(f"{package}-0.3.3/{name}")
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
            (self.root / "scripts" / package).mkdir()
            (self.root / "scripts/patches" / package).mkdir(parents=True)
            expected = self.root / "expected" / package
            for name, data in files.items():
                target = expected / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            entries = []
            for patch_name, changed in PATCHES[package]:
                patch = self.root / "scripts/patches" / package / patch_name
                before = files[changed].decode()
                patch.write_text(diff(changed, before, "patched " + before))
                (expected / changed).write_text("patched " + before)
                entries.append({"path": f"../patches/{package}/{patch_name}", "sha256": setup.digest(patch.read_bytes())})
            self.manifests[package] = {
                "package": package, "version": "0.3.3", "archive_url": "must-never-access-network",
                "archive_sha256": setup.digest((self.archives / f"{package}-0.3.3.crate").read_bytes()),
                "patches": entries, "tree_sha256": setup.tree_digest(expected),
                "generated_path": f"vendor/{package}-0.3.3",
                "changed_files": sorted(changed for _, changed in PATCHES[package]),
            }
            self.write_manifest(package)

    def write_manifest(self, package):
        (self.root / "scripts" / package / "manifest.json").write_text(json.dumps(self.manifests[package]))

    def run_setup(self, *args, success=True):
        result = subprocess.run([sys.executable, str(self.root / "scripts/gpui-pre-apple-setup.py"),
                                 "--archive-dir", str(self.archives), *args], text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result.stdout + result.stderr

    def test_recreates_both_crates_in_patch_order_is_idempotent_then_rejects_drift(self):
        self.run_setup()
        window = self.root / "vendor/gpui-pre-macos-0.3.3/src/window.rs"
        self.assertEqual(window.read_bytes(), b"patched window\n")
        self.assertEqual((self.root / "vendor/gpui-pre-macos-0.3.3/src/platform.rs").read_bytes(), b"patched platform\n")
        self.assertEqual((self.root / "vendor/gpui-pre-apple-0.3.3/src/metal_renderer.rs").read_bytes(), b"patched renderer\n")
        before = window.stat().st_mtime_ns
        self.run_setup()
        self.run_setup("--verify")
        self.assertEqual(window.stat().st_mtime_ns, before)
        window.write_text("unexpected mutation\n")
        self.assertIn("generated gpui-pre-macos differs", self.run_setup("--verify", success=False))

    def test_missing_generated_source_is_not_a_successful_verification(self):
        self.assertIn("generated gpui-pre-macos missing", self.run_setup("--verify", success=False))

    def test_archive_and_patch_checksums_are_not_advisory(self):
        archive = self.archives / "gpui-pre-apple-0.3.3.crate"
        original = archive.read_bytes()
        archive.write_bytes(original + b"tampered")
        self.assertIn("gpui-pre-apple archive checksum mismatch", self.run_setup(success=False))
        archive.write_bytes(original)
        patch = self.root / "scripts/patches/gpui-pre-macos/0002.diff"
        patch.write_text(patch.read_text() + "\n")
        self.assertIn("gpui-pre-macos patch/manifest mismatch", self.run_setup(success=False))

    def test_even_a_rehashed_patch_cannot_change_an_unapproved_file(self):
        patch = self.root / "scripts/patches/gpui-pre-apple/0001.diff"
        patch.write_text(patch.read_text() + diff("Cargo.toml", "apple\n", "unapproved change\n"))
        self.manifests["gpui-pre-apple"]["patches"][0]["sha256"] = setup.digest(patch.read_bytes())
        self.write_manifest("gpui-pre-apple")
        self.assertIn("not the approved", self.run_setup(success=False))


if __name__ == "__main__":
    unittest.main()
