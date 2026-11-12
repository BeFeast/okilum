"""Offline controls for the checksum-pinned, DirectX-only vendor exception."""
import difflib
import hashlib
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
spec = importlib.util.spec_from_file_location("atlas_setup", ROOT / "scripts/gpui-pre-windows-setup.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class WindowsAtlasSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="atlas-vendor-control-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts/gpui-pre-windows").mkdir(parents=True)
        (self.root / "scripts/patches/gpui-pre-windows").mkdir(parents=True)
        shutil.copy2(ROOT / "scripts/gpui-pre-windows-setup.py", self.root / "scripts/gpui-pre-windows-setup.py")
        self.files = {"Cargo.toml": b"unchanged package\n", "src/directx_atlas.rs": b"before\n"}
        self.archive = self.root / "source.crate"
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, data in self.files.items():
                info = tarfile.TarInfo("gpui-pre-windows-0.3.3/" + name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        expected = self.root / "expected"
        for name, data in {**self.files, "src/directx_atlas.rs": b"after\n"}.items():
            target = expected / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        self.patch = self.root / "scripts/patches/gpui-pre-windows/0001.diff"
        self.patch.write_text("".join(difflib.unified_diff(["before\n"], ["after\n"], fromfile="a/src/directx_atlas.rs", tofile="b/src/directx_atlas.rs")))
        self.manifest = {"version": "0.3.3", "archive_url": "must-never-access-network", "archive_sha256": setup.digest(self.archive.read_bytes()), "patch": "../patches/gpui-pre-windows/0001.diff", "patch_sha256": setup.digest(self.patch.read_bytes()), "tree_sha256": setup.tree_digest(expected), "generated_path": "vendor/gpui-pre-windows-0.3.3", "changed_files": ["src/directx_atlas.rs"]}
        self.write_manifest()

    def write_manifest(self):
        (self.root / "scripts/gpui-pre-windows/manifest.json").write_text(json.dumps(self.manifest))

    def run_setup(self, *args, success=True):
        result = subprocess.run([sys.executable, str(self.root / "scripts/gpui-pre-windows-setup.py"), "--archive", str(self.archive), *args], text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result.stdout + result.stderr

    def test_recreates_exact_source_and_is_idempotent_then_rejects_drift(self):
        self.run_setup()
        source = self.root / "vendor/gpui-pre-windows-0.3.3/src/directx_atlas.rs"
        self.assertEqual(source.read_bytes(), b"after\n")
        before = source.stat().st_mtime_ns
        self.run_setup()
        self.run_setup("--verify")
        self.assertEqual(source.stat().st_mtime_ns, before)
        source.write_text("unexpected mutation\n")
        self.assertIn("differs from pinned source+patch", self.run_setup("--verify", success=False))

    def test_missing_generated_source_is_not_a_successful_verification(self):
        self.assertIn("generated gpui-pre-windows missing", self.run_setup("--verify", success=False))

    def test_archive_and_patch_checksums_are_not_advisory(self):
        original = self.archive.read_bytes()
        self.archive.write_bytes(original + b"tampered")
        self.assertIn("archive checksum mismatch", self.run_setup(success=False))
        self.archive.write_bytes(original)
        self.patch.write_text(self.patch.read_text() + "\n")
        self.assertIn("patch/manifest mismatch", self.run_setup(success=False))

    def test_even_a_rehashed_patch_cannot_change_a_second_backend_file(self):
        self.patch.write_text(self.patch.read_text() + "".join(difflib.unified_diff(["unchanged package\n"], ["unapproved change\n"], fromfile="a/Cargo.toml", tofile="b/Cargo.toml")))
        self.manifest["patch_sha256"] = setup.digest(self.patch.read_bytes())
        self.write_manifest()
        self.assertIn("exceeds the DirectX atlas-upload exception", self.run_setup(success=False))


if __name__ == "__main__":
    unittest.main()
