"""Phase receipt reporting. No compiler and no build-slot."""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PHASE = ROOT / "scripts/ci/pool-phase.sh"


class PoolGit(unittest.TestCase):
    def test_an_archived_tree_becomes_a_repository_git_ls_files_can_read(self):
        lib = ROOT / "scripts/ci/pool-phase-lib.sh"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "tracked.txt").write_text("ok\n")
            (path / ".gitignore").write_text("ignored.txt\n")
            (path / "ignored.txt").write_text("no\n")
            script = f'set -euo pipefail\nsource "{lib}"\npool_ensure_git\ngit ls-files'
            result = subprocess.run(["bash", "-c", script], cwd=path, capture_output=True,
                                    text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("tracked.txt", result.stdout)
            self.assertNotIn("ignored.txt", result.stdout)

    def test_host_excludesfile_cannot_drop_archived_source(self):
        lib = ROOT / "scripts/ci/pool-phase-lib.sh"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            home = root / "home"
            work = root / "work"
            home.mkdir()
            work.mkdir()
            (home / ".config/git").mkdir(parents=True)
            (home / ".config/git/ignore").write_text("**/credentials.*\n")
            (work / "credentials.rs").write_text("mod credentials;\n")
            (work / "lib.rs").write_text("ok\n")
            script = f'set -euo pipefail\nsource "{lib}"\npool_ensure_git\ngit ls-files'
            result = subprocess.run(["bash", "-c", script], cwd=work, capture_output=True, text=True,
                                    timeout=30, env={"HOME": str(home), "PATH": os.environ["PATH"]})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("credentials.rs", result.stdout)
            # A reused slot keeps the synthetic repository. Rebuilding it must
            # pick up a file the old index omitted.
            (work / "extra.rs").write_text("extra\n")
            again = subprocess.run(["bash", "-c", script], cwd=work, capture_output=True, text=True,
                                   timeout=30, env={"HOME": str(home), "PATH": os.environ["PATH"]})
            self.assertEqual(again.returncode, 0, again.stderr)
            self.assertIn("extra.rs", again.stdout)
            self.assertIn("credentials.rs", again.stdout)

    def test_a_stale_slot_index_is_rebuilt_when_the_dispatcher_names_the_commit(self):
        lib = ROOT / "scripts/ci/pool-phase-lib.sh"
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / "keep.rs").write_text("keep\n")
            subprocess.run(["git", "init", "-q"], cwd=work, check=True, capture_output=True)
            subprocess.run(["git", "-c", "user.email=pool@okilum.local", "-c", "user.name=pool",
                            "add", "keep.rs"], cwd=work, check=True, capture_output=True)
            subprocess.run(["git", "-c", "user.email=pool@okilum.local", "-c", "user.name=pool",
                            "commit", "-q", "-m", "old checkout"], cwd=work, check=True, capture_output=True)
            (work / "keep.rs").unlink()
            (work / "now.rs").write_text("now\n")
            script = f'set -euo pipefail\nsource "{lib}"\npool_ensure_git\ngit ls-files'
            result = subprocess.run(["bash", "-c", script], cwd=work, capture_output=True, text=True,
                                    timeout=30, env={**os.environ, "OKILUM_SOURCE_COMMIT": "d" * 40})
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("now.rs", result.stdout)
            self.assertNotIn("keep.rs", result.stdout)


class PoolPhase(unittest.TestCase):
    def test_only_a_recorded_success_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "phases.json"
            receipt.write_text(json.dumps([
                {"name": "Run Linux Rust suites offline", "code": 0},
                {"name": "Lint every workspace target offline", "code": 1},
            ]))
            env = {"OKILUM_POOL_RECEIPT": str(receipt), "PATH": "/usr/bin:/bin"}
            ok = subprocess.run(["bash", str(PHASE), "Run Linux Rust suites offline"], env=env,
                                capture_output=True, text=True, timeout=30)
            bad = subprocess.run(["bash", str(PHASE), "Lint every workspace target offline"], env=env,
                                 capture_output=True, text=True, timeout=30)
            missing = subprocess.run(["bash", str(PHASE), "Not a phase"], env=env,
                                     capture_output=True, text=True, timeout=30)
            self.assertEqual(ok.returncode, 0, ok.stderr)
            self.assertEqual(bad.returncode, 1)
            self.assertIn("failed", bad.stderr)
            self.assertEqual(missing.returncode, 1)
            self.assertIn("not run", missing.stderr)


if __name__ == "__main__":
    unittest.main()
