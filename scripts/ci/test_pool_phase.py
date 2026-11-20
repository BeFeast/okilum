"""Phase receipt reporting. No compiler and no build-slot."""
import json
import os
import re
import runpy
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


class PoolVendor(unittest.TestCase):
    """A reused slot keeps ignored vendor/ output; a changed manifest must not."""

    def run_lib(self, work, mode="", commit="abc"):
        lib = ROOT / "scripts/ci/pool-phase-lib.sh"
        env = {"PATH": os.environ["PATH"], "HOME": os.environ.get("HOME", "/tmp")}
        if commit:
            env["OKILUM_SOURCE_COMMIT"] = commit
        script = f'set -euo pipefail\nsource "{lib}"\npool_reset_stale_vendor {mode}'
        result = subprocess.run(["bash", "-c", script], cwd=work, capture_output=True,
                                text=True, timeout=30, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_only_a_tree_for_a_changed_manifest_is_regenerated(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            for name in ("a", "b"):
                (work / "scripts" / name).mkdir(parents=True)
                (work / "scripts" / name / "manifest.json").write_text(
                    json.dumps({"generated_path": f"vendor/{name}-1", "tree_sha256": "1"}))
                (work / "vendor" / f"{name}-1").mkdir(parents=True)
                (work / "vendor" / f"{name}-1" / "lib.rs").write_text("old\n")
            # First run in a slot: nothing verified yet, so both regenerate.
            self.run_lib(work)
            self.assertFalse((work / "vendor/a-1").exists())
            self.assertFalse((work / "vendor/b-1").exists())
            for name in ("a", "b"):
                (work / "vendor" / f"{name}-1").mkdir()
                (work / "vendor" / f"{name}-1" / "lib.rs").write_text("new\n")
            self.run_lib(work, "stamp")
            # Unchanged manifests keep their trees.
            self.run_lib(work)
            self.assertTrue((work / "vendor/a-1/lib.rs").exists())
            self.assertTrue((work / "vendor/b-1/lib.rs").exists())
            # main changes a's patches: only a is removed, b stays warm.
            (work / "scripts/a/manifest.json").write_text(
                json.dumps({"generated_path": "vendor/a-1", "tree_sha256": "2"}))
            out = self.run_lib(work)
            self.assertFalse((work / "vendor/a-1").exists())
            self.assertTrue((work / "vendor/b-1/lib.rs").exists())
            self.assertIn("vendor/a-1", out.stdout)
            # A failed regeneration leaves no stamp, so a partial tree is removed again.
            (work / "vendor/a-1").mkdir()
            self.run_lib(work)
            self.assertFalse((work / "vendor/a-1").exists())

    def test_a_developer_clone_is_untouched(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / "scripts/a").mkdir(parents=True)
            (work / "scripts/a/manifest.json").write_text(json.dumps({"generated_path": "vendor/a-1"}))
            (work / "vendor/a-1").mkdir(parents=True)
            self.run_lib(work, commit="")
            self.assertTrue((work / "vendor/a-1").exists())


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


class PoolShardedPhase(unittest.TestCase):
    def check(self, phases, name):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "phases.json"
            receipt.write_text(json.dumps(phases))
            return subprocess.run(["bash", str(PHASE), name], capture_output=True, text=True, timeout=30,
                                  env={"OKILUM_POOL_RECEIPT": str(receipt), "PATH": "/usr/bin:/bin"})

    def test_a_phase_passes_only_when_every_shard_record_passed(self):
        name = "Run Linux Rust suites offline"
        ok = self.check([{"name": name, "code": 0, "shard": "rust-a"},
                         {"name": name, "code": 0, "shard": "rust-b"}], name)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("rust-a, rust-b", ok.stdout)
        # The later record passing must not hide an earlier shard's failure.
        bad = self.check([{"name": name, "code": 1, "shard": "rust-a"},
                          {"name": name, "code": 0, "shard": "rust-b"}], name)
        self.assertEqual(bad.returncode, 1)
        self.assertIn("on shard rust-a", bad.stderr)


AGGREGATE = runpy.run_path(str(ROOT / "scripts/ci/pool-aggregate.py"))
COMPLETE = {"name": "Pool shard complete", "code": 0}


class PoolAggregate(unittest.TestCase):
    def shard(self, root, name, phases=None, rc=0, oom=0, slot=True):
        directory = Path(root) / name
        directory.mkdir()
        if phases is not None:
            (directory / "phases.json").write_text(json.dumps(phases))
        if rc is not None:
            (directory / "rc").write_text(f"{rc}\n")
        if slot:
            (directory / "build-slot.json").write_text(json.dumps(
                {"host": "baldr", "oom_kill": oom, "queue_waited_s": 3, "ran_s": 60}))
        return str(directory)

    def test_every_shard_passing_is_a_verdict_with_all_records(self):
        with tempfile.TemporaryDirectory() as root:
            a = self.shard(root, "base", [{"name": "x", "code": 0}, COMPLETE])
            b = self.shard(root, "sync", [{"name": "x", "code": 0}, {"name": "y", "code": 0}, COMPLETE])
            kind, merged, shards = AGGREGATE["aggregate"]([a, b])
            self.assertEqual(kind, "verdict")
            self.assertEqual([(m["name"], m["shard"]) for m in merged],
                             [("x", "base"), ("x", "sync"), ("y", "sync")])
            self.assertEqual({s["state"] for s in shards}, {"pass"})

    def test_a_lost_shard_without_a_code_failure_is_infrastructure(self):
        with tempfile.TemporaryDirectory() as root:
            a = self.shard(root, "base", [{"name": "x", "code": 0}, COMPLETE])
            queued = self.shard(root, "sync", None, rc=75, slot=False)
            self.assertEqual(AGGREGATE["aggregate"]([a, queued])[0], "infra")

    def test_a_shard_that_stopped_before_its_last_record_is_not_a_pass(self):
        with tempfile.TemporaryDirectory() as root:
            truncated = self.shard(root, "base", [{"name": "x", "code": 0}], rc=0)
            self.assertEqual(AGGREGATE["aggregate"]([truncated])[0], "infra")

    def test_a_failing_phase_is_a_code_verdict_even_when_another_shard_was_lost(self):
        with tempfile.TemporaryDirectory() as root:
            failed = self.shard(root, "desktop", [{"name": "x", "code": 101},
                                                  {"name": "Pool shard complete", "code": 101}], rc=1)
            lost = self.shard(root, "rust-b", None, rc=255, slot=False)
            kind, merged, _ = AGGREGATE["aggregate"]([failed, lost])
            self.assertEqual(kind, "verdict")
            self.assertEqual(merged, [{"name": "x", "code": 101, "shard": "desktop"}])

    def test_a_phase_deadline_is_a_verdict_but_an_oom_kill_is_not(self):
        with tempfile.TemporaryDirectory() as root:
            deadline = self.shard(root, "admission", [{"name": "x", "code": 124}], rc=124)
            self.assertEqual(AGGREGATE["aggregate"]([deadline])[0], "verdict")
            oom = self.shard(root, "rust-a", [{"name": "x", "code": 137}], rc=1, oom=1)
            self.assertEqual(AGGREGATE["aggregate"]([oom])[0], "infra")
            # build-slot's own timeout with every recorded phase green is no verdict.
            slot_timeout = self.shard(root, "sync", [{"name": "x", "code": 0}], rc=124)
            self.assertEqual(AGGREGATE["aggregate"]([slot_timeout])[0], "infra")


class PoolShardSelection(unittest.TestCase):
    def test_admission_parts_cover_every_parent_test_once(self):
        runs = runpy.run_path(str(ROOT / "scripts/ci/sync-managed-admission-runs.py"), run_name="admission")
        tests, crash, checkpoints = runs["TESTS"], runs["CRASH_TEST"], runs["CHECKPOINTS"]
        full = runs["select"]("all")
        self.assertEqual([name for name, _, _ in full], ["crash-even", "crash-odd", "remaining"])
        crash_part = runs["select"]("crash")
        self.assertEqual(sorted(c for _, part, _ in crash_part for c in part), sorted(checkpoints))
        zero = runs["select"]("remaining-0")[0][2]
        one = runs["select"]("remaining-1")[0][2]
        self.assertFalse(zero & one)
        self.assertEqual(zero | one, tests - {crash})
        with self.assertRaises(SystemExit):
            runs["select"]("remaining-2")

    def test_rust_parts_split_the_workspace_census_from_the_other_groups(self):
        source = (ROOT / "scripts/ci/linux-rust-tests.sh").read_text()
        self.assertIn("suite workspace-full ", source)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts/ci").mkdir(parents=True)
            (root / "scripts/ci/linux-rust-tests.sh").write_text(source)
            (root / "scripts/ci/test-nofile.sh").write_text("")
            (root / "scripts/ci/test-group.sh").write_text('shift 2\necho "RAN $*" >> "$LOG_FILE"\n')
            for part, workspace, others in (("groups", False, True), ("workspace", True, False),
                                            ("all", True, True)):
                log = root / f"{part}.log"
                result = subprocess.run(["bash", "scripts/ci/linux-rust-tests.sh"], cwd=root,
                                        capture_output=True, text=True, timeout=60,
                                        env={**os.environ, "OKILUM_LINUX_RUST_PART": part, "LOG_FILE": str(log)})
                self.assertEqual(result.returncode, 0, result.stderr)
                ran = log.read_text()
                self.assertEqual("workspace-rust-tests.py" in ran, workspace, part)
                self.assertEqual("local-writer-admission.sh" in ran, others, part)
            bad = subprocess.run(["bash", "scripts/ci/linux-rust-tests.sh"], cwd=root, capture_output=True,
                                 text=True, timeout=60, env={**os.environ, "OKILUM_LINUX_RUST_PART": "x"})
            self.assertEqual(bad.returncode, 64)

    def test_every_required_linux_phase_runs_on_some_shard(self):
        gate = (ROOT / "scripts/ci/pool-linux-gate.sh").read_text()
        dispatch = (ROOT / "scripts/ci/pool-dispatch.sh").read_text()
        for shard in ("base", "admission", "sync", "desktop", "rust-a", "rust-b"):
            self.assertIn(f'"{shard} ', dispatch)
        workflow = (ROOT / ".github/workflows/agent-checks.yml").read_text()
        linux = workflow[workflow.index("\n  linux:"):]
        for step in re.findall(r'pool-phase\.sh "([^"]+)"', linux):
            self.assertIn(f'pool_phase "{step}"', gate, step)


if __name__ == "__main__":
    unittest.main()
