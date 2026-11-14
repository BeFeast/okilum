"""Offline controls for scripts/velopack-setup.py: a stale generated tree is
regenerated from the pinned archive and patch, never trusted or left in place."""
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
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("velopack_setup", ROOT / "scripts/velopack-setup.py")
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)

FILES = {"Cargo.toml": b"[package]\nname = \"velopack\"\n", "src/lib.rs": b"pub fn helper() {}\n"}


def diff(name, before, after):
    return "".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                        fromfile="a/" + name, tofile="b/" + name, n=0))


class VelopackSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="velopack-vendor-control-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts/velopack").mkdir(parents=True)
        shutil.copy2(ROOT / "scripts/velopack-setup.py", self.root / "scripts/velopack-setup.py")
        self.archive = self.root / "velopack-1.2.161.crate"
        with tarfile.open(self.archive, "w:gz") as archive:
            for name, data in FILES.items():
                info = tarfile.TarInfo("velopack-1.2.161/" + name)
                info.size = len(data)
                info.mode = 0o644
                archive.addfile(info, io.BytesIO(data))
        self.dest = self.root / "vendor/velopack-1.2.161"
        self.pin("pub fn helper() { /* v1 */ }\n")

    def pin(self, patched):
        """Write a patch and manifest the way an updater PR changes them."""
        before = FILES["src/lib.rs"].decode()
        patch = self.root / "scripts/velopack/hidden-delta.diff"
        patch.write_text(diff("src/lib.rs", before, patched))
        expected = self.root / "expected"
        shutil.rmtree(expected, ignore_errors=True)
        for name, data in FILES.items():
            (expected / name).parent.mkdir(parents=True, exist_ok=True)
            (expected / name).write_bytes(data)
        (expected / "src/lib.rs").write_text(patched)
        self.manifest = {
            "package": "velopack", "version": "1.2.161", "archive_url": "must-never-access-network",
            "archive_sha256": setup.digest(self.archive.read_bytes()),
            "patch": "hidden-delta.diff", "patch_sha256": setup.digest(patch.read_bytes()),
            "tree_sha256": setup.tree_digest(expected), "generated_path": "vendor/velopack-1.2.161",
        }
        self.write_manifest()

    def write_manifest(self):
        (self.root / "scripts/velopack/manifest.json").write_text(json.dumps(self.manifest))

    def run_setup(self, *args, success=True):
        result = subprocess.run([sys.executable, str(self.root / "scripts/velopack-setup.py"),
                                 "--archive", str(self.archive), *args],
                                cwd=self.root, text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result.stdout + result.stderr

    def leftovers(self):
        return [p.name for p in self.dest.parent.iterdir() if p.name.startswith(".velopack-")]

    def test_fresh_generation_and_verify(self):
        self.run_setup("--verify", success=False)
        self.assertIn("velopack ready", self.run_setup())
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v1 */ }\n")
        self.assertIn("velopack ready", self.run_setup("--verify"))
        self.assertEqual(self.leftovers(), [])

    def test_stale_tree_from_an_older_patch_is_regenerated(self):
        self.run_setup()
        (self.dest / "obsolete.rs").write_text("left by the old patch\n")
        self.pin("pub fn helper() { /* v2 */ }\n")
        # --verify still refuses the stale tree; it never rewrites anything.
        self.assertIn("differs from pinned", self.run_setup("--verify", success=False))
        self.assertTrue((self.dest / "obsolete.rs").exists())
        output = self.run_setup()
        self.assertIn("stale for the current patch; regenerating", output)
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v2 */ }\n")
        self.assertFalse((self.dest / "obsolete.rs").exists())
        self.assertEqual(setup.tree_digest(self.dest), self.manifest["tree_sha256"])
        self.run_setup("--verify")
        self.assertEqual(self.leftovers(), [])

    def test_regeneration_still_requires_the_pinned_archive(self):
        self.run_setup()
        self.pin("pub fn helper() { /* v2 */ }\n")
        self.manifest["archive_sha256"] = "0" * 64
        self.write_manifest()
        self.assertIn("archive checksum mismatch", self.run_setup(success=False))
        # The stale tree is kept untouched when the replacement cannot be verified.
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v1 */ }\n")
        self.assertEqual(self.leftovers(), [])

    def test_regeneration_still_requires_the_pinned_tree(self):
        self.run_setup()
        self.pin("pub fn helper() { /* v2 */ }\n")
        self.manifest["tree_sha256"] = "0" * 64
        self.write_manifest()
        self.assertIn("generated source/manifest mismatch", self.run_setup(success=False))
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v1 */ }\n")
        self.assertEqual(self.leftovers(), [])

    def test_patch_manifest_mismatch_is_never_regenerated(self):
        self.run_setup()
        (self.root / "scripts/velopack/hidden-delta.diff").write_text("tampered\n")
        self.assertIn("patch/manifest mismatch", self.run_setup(success=False))
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v1 */ }\n")

    def interrupt_rename(self, call):
        """Raise KeyboardInterrupt on the call-th Path.rename (1-based)."""
        original = Path.rename
        calls = []

        def rename(path, target):
            calls.append((Path(path).name, Path(target).name))
            if len(calls) == call:
                raise KeyboardInterrupt
            return original(path, target)
        return mock.patch.object(Path, "rename", rename), calls

    def generate_in_process(self):
        patch = self.root / "scripts/velopack/hidden-delta.diff"
        setup.generate(self.manifest, patch, self.dest, self.archive)

    def test_interrupt_between_the_renames_keeps_the_old_tree(self):
        self.run_setup()
        self.pin("pub fn helper() { /* v2 */ }\n")
        # Rename 1: old tree -> backup; rename 2: new tree -> dest (interrupted).
        patcher, calls = self.interrupt_rename(2)
        with patcher, self.assertRaises(KeyboardInterrupt):
            self.generate_in_process()
        self.assertEqual(calls[0], ("velopack-1.2.161", ".velopack-1.2.161.previous"))
        self.assertEqual(calls[1][1], "velopack-1.2.161")
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v1 */ }\n")
        self.assertEqual(self.leftovers(), [])
        self.assertIn("regenerating", self.run_setup())
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v2 */ }\n")

    def test_interrupt_before_any_rename_keeps_the_old_tree(self):
        self.run_setup()
        self.pin("pub fn helper() { /* v2 */ }\n")
        patcher, _ = self.interrupt_rename(1)
        with patcher, self.assertRaises(KeyboardInterrupt):
            self.generate_in_process()
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v1 */ }\n")
        self.assertEqual(self.leftovers(), [])

    def test_backup_left_by_a_killed_swap_is_recovered(self):
        # A SIGKILL between the renames leaves only the backup; nothing is lost.
        self.run_setup()
        self.dest.rename(self.dest.with_name(".velopack-1.2.161.previous"))
        self.pin("pub fn helper() { /* v2 */ }\n")
        self.assertIn("velopack ready", self.run_setup())
        self.assertEqual((self.dest / "src/lib.rs").read_text(), "pub fn helper() { /* v2 */ }\n")
        self.assertEqual(self.leftovers(), [])

    def test_symlinked_stale_tree_is_refused(self):
        elsewhere = self.root / "elsewhere"
        elsewhere.mkdir()
        (elsewhere / "keep.txt").write_text("not ours\n")
        self.dest.parent.mkdir(parents=True)
        self.dest.symlink_to(elsewhere, target_is_directory=True)
        self.assertIn("not a plain directory", self.run_setup(success=False))
        self.assertEqual((elsewhere / "keep.txt").read_text(), "not ours\n")


if __name__ == "__main__":
    unittest.main()
