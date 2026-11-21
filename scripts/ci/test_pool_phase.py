"""Phase receipt reporting. No compiler and no build-slot."""
import json
import os
import re
import runpy
import subprocess
import sys
import tempfile
import time
import shutil
import unittest
import unittest.mock
from pathlib import Path

import yaml

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


class PoolHostedEnv(unittest.TestCase):
    def test_each_run_gets_private_state_umask_022_and_no_group_writable_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "target" / "debug"
            target.mkdir(parents=True)
            helper = target / "okilum-sync-launch"
            helper.write_text("")
            helper.chmod(0o775)
            receipt = root / "r" / "phases.json"
            script = (f'source "{ROOT}/scripts/ci/pool-phase-lib.sh"; pool_receipt_init; '
                      'umask; printf "%s\\n%s\\n" "$XDG_STATE_HOME" "$XDG_DATA_HOME"; '
                      ': > "$XDG_STATE_HOME/x"; touch fresh; stat -c %a fresh')
            env = {**os.environ, "TMPDIR": str(root), "CARGO_TARGET_DIR": str(root / "target"),
                   "OKILUM_POOL_RECEIPT": str(receipt), "XDG_STATE_HOME": str(root / "ambient")}
            runs = [subprocess.run(["bash", "-c", script], cwd=root, env=env, capture_output=True,
                                   text=True, check=True).stdout.split() for _ in range(2)]
            for umask, state, data, fresh in runs:
                self.assertEqual(umask, "0022")
                self.assertEqual(fresh, "644")
                self.assertTrue(state.startswith(str(root / "okilum-pool-home.")), state)
                self.assertTrue(Path(state).is_dir() and Path(data).is_dir())
            self.assertNotEqual(runs[0][1], runs[1][1])
            self.assertFalse((root / "ambient").exists())
            self.assertEqual(helper.stat().st_mode & 0o777, 0o755)


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


class PoolOverall(unittest.TestCase):
    def overall(self, phases, shards):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / "phases.json"
            receipt.write_text(json.dumps(phases))
            env = {"OKILUM_POOL_RECEIPT": str(receipt), "PATH": "/usr/bin:/bin"}
            if shards is not None:
                (Path(directory) / "shards.json").write_text(json.dumps(shards))
                env["OKILUM_POOL_SHARDS"] = str(Path(directory) / "shards.json")
            return subprocess.run(["bash", str(PHASE), "--overall"], capture_output=True, text=True,
                                  timeout=30, env=env)

    def test_only_every_shard_and_record_passing_is_green(self):
        passed = [{"shard": "base", "state": "pass", "rc": 0}, {"shard": "sync", "state": "pass", "rc": 0}]
        ok = self.overall([{"name": "x", "code": 0, "shard": "base"}], passed)
        self.assertEqual(ok.returncode, 0, ok.stderr)
        for phases, shards, why in (
                ([{"name": "x", "code": 1, "shard": "sync"}], passed, "exit 1"),
                ([{"name": "x", "code": None, "skipped": True, "shard": "sync"}], passed, "skipped"),
                ([{"name": "x", "shard": "sync"}], passed, "no exit code"),
                ([{"name": "x", "code": 0}], [passed[0], {"shard": "sync", "state": "fail", "rc": 1}],
                 "pool shard sync fail"),
                ([{"name": "x", "code": 0}], [passed[0], {"shard": "rust-b", "state": "infra", "rc": 75}],
                 "pool shard rust-b infra"),
                ([{"name": "x", "code": 0}], [], "no pool shard ran"),
                ([{"name": "x", "code": 0}], None, "OKILUM_POOL_SHARDS is unset")):
            result = self.overall(phases, shards)
            self.assertEqual(result.returncode, 1, why)
            self.assertIn(why, result.stderr)


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
        # A shard that stopped before this phase cannot borrow another shard's pass.
        skipped = self.check([{"name": name, "code": 0, "shard": "rust-a"},
                              {"name": name, "code": None, "skipped": True, "shard": "rust-b"}], name)
        self.assertEqual(skipped.returncode, 1)
        self.assertIn("failed on shard rust-b: skipped after an earlier failure", skipped.stderr)


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
            record = {"host": "baldr", "exit": rc, "oom_kill": oom, "queue_waited_s": 3, "ran_s": 60}
            if isinstance(slot, dict):
                record.update(slot)
                record = {key: value for key, value in record.items() if value is not ...}
            (directory / "build-slot.json").write_text(json.dumps(record))
        return str(directory)

    def test_a_run_without_a_complete_slot_receipt_is_never_a_verdict(self):
        """OOM status comes only from build-slot's receipt (#3186 review)."""
        green = [{"name": "x", "code": 0}, COMPLETE]
        red = [{"name": "x", "code": 1}, {"name": "Pool shard complete", "code": 1}]
        cases = {
            "missing": dict(slot=False),
            "no-oom": dict(slot={"oom_kill": ...}),
            "null-oom": dict(slot={"oom_kill": None}),
            "text-oom": dict(slot={"oom_kill": "0"}),
            "negative-oom": dict(slot={"oom_kill": -1}),
            "no-exit": dict(slot={"exit": ...}),
            "other-exit": dict(slot={"exit": 75}),
        }
        with tempfile.TemporaryDirectory() as root:
            for name, options in cases.items():
                for phases, rc in ((green, 0), (red, 1)):
                    shard = self.shard(root, f"{name}-{rc}", phases, rc=rc, **options)
                    state = AGGREGATE["shard_state"](shard)[0]
                    self.assertEqual(state, "infra", (name, rc))
                    self.assertEqual(AGGREGATE["aggregate"]([shard])[0], "infra", (name, rc))
            invalid = self.shard(root, "invalid", green, rc=0, slot=False)
            Path(invalid, "build-slot.json").write_text("{not json")
            self.assertEqual(AGGREGATE["shard_state"](invalid)[0], "infra")
            listed = self.shard(root, "listed", green, rc=0, slot=False)
            Path(listed, "build-slot.json").write_text("[]")
            self.assertEqual(AGGREGATE["shard_state"](listed)[0], "infra")
            # Controls: the same receipts with explicit metadata classify as before.
            self.assertEqual(AGGREGATE["shard_state"](self.shard(root, "ok", green, rc=0))[0], "pass")
            self.assertEqual(AGGREGATE["shard_state"](self.shard(root, "bad", red, rc=1))[0], "fail")

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
            # The workspace part precompiles the census selection outside a suite.
            (root / "bin").mkdir()
            (root / "bin/cargo").write_text('#!/bin/sh\necho "CARGO $*" >> "$LOG_FILE"\n')
            (root / "bin/cargo").chmod(0o755)
            path = f"{root / 'bin'}{os.pathsep}{os.environ['PATH']}"
            # Pool shards pick the part through the environment, the hosted gate
            # through the first argument (#3198); the argument wins.
            for part, args, workspace, others in (
                    ("checks", [], False, True), ("workspace", [], True, False), ("all", [], True, True),
                    ("x", ["checks"], False, True), ("all", ["workspace"], True, False)):
                log = root / f"{part}-{'-'.join(args)}.log"
                result = subprocess.run(["bash", "scripts/ci/linux-rust-tests.sh", *args], cwd=root,
                                        capture_output=True, text=True, timeout=60,
                                        env={**os.environ, "PATH": path, "OKILUM_LINUX_RUST_PART": part,
                                             "LOG_FILE": str(log)})
                self.assertEqual(result.returncode, 0, result.stderr)
                ran = log.read_text()
                self.assertEqual("workspace-rust-tests.py" in ran, workspace, (part, args))
                self.assertEqual("local-writer-admission.sh" in ran, others, (part, args))
            for env_part, args in (("x", []), ("all", ["groups"])):
                bad = subprocess.run(["bash", "scripts/ci/linux-rust-tests.sh", *args], cwd=root,
                                     capture_output=True, text=True, timeout=60,
                                     env={**os.environ, "OKILUM_LINUX_RUST_PART": env_part})
                self.assertEqual(bad.returncode, 2, (env_part, args))

    def test_every_required_linux_phase_runs_on_some_shard(self):
        gate = (ROOT / "scripts/ci/pool-linux-gate.sh").read_text()
        dispatch = (ROOT / "scripts/ci/pool-dispatch.sh").read_text()
        for shard in ("base", "admission", "sync", "desktop", "rust-a", "rust-b"):
            self.assertIn(f'"{shard} ', dispatch)
        workflow = (ROOT / ".github/workflows/agent-checks.yml").read_text()
        linux = workflow[workflow.index("\n  linux:"):]
        for step in re.findall(r'pool-phase\.sh "([^"]+)"', linux):
            self.assertIn(f'pool_phase "{step}"', gate, step)



class PoolHostDiscoveryIsolation(unittest.TestCase):
    """#3186 review: a sibling slot's same-uid Syncthing must not reach host discovery."""

    def test_pool_admission_isolates_host_discovery_on_probed_pidns_hosts(self):
        runs = (ROOT / "scripts/ci/sync-managed-admission-runs.py").read_text()
        self.assertIn("pidns['probe'](work)", runs)
        self.assertIn("pidns['command'](command, env, root, guard=root)", runs)
        self.assertIn("assert not foreign_syncthing()", runs)  # unchanged outside the pool
        dispatch = (ROOT / "scripts/ci/pool-dispatch.sh").read_text()
        # The probed capability, not plain sudo (okilum-dev has sudo but refuses the /proc mount).
        self.assertRegex(dispatch, r'"admission \d+ [a-z,]*\bpidns\b')
        self.assertRegex(dispatch, r'"all \d+ [a-z,]*\bpidns\b')
        self.assertNotRegex(dispatch, r'"(pre-merge|base|rust-a|rust-b|sync|desktop) \d+ [a-z,]*\bpidns\b')
        gate = (ROOT / "scripts/ci/pool-linux-gate.sh").read_text()
        lib = (ROOT / "scripts/ci/pool-phase-lib.sh").read_text()
        probe = lib[lib.index("pool_require_pidns() {"):]
        self.assertIn("sudo -n unshare --pid --fork --kill-child --mount-proc -- true", probe[:probe.index("\n}\n")])
        self.assertIn("    admission|all) pool_require_pidns ;;", gate)

    def test_namespace_controls_run_only_in_the_admission_shard(self):
        """Pre-merge and base exclude them from discovery by name; admission runs them, mandatory."""
        exclude = "OKILUM_UNITTEST_EXCLUDE=test_pid_namespace python3 -I -m unittest discover -s scripts/ci"
        premerge = (ROOT / "scripts/ci/pool-pre-merge.sh").read_text()
        gate = (ROOT / "scripts/ci/pool-linux-gate.sh").read_text()
        base = gate[gate.index("if has base; then\n    pool_phase \"Verify agent helper controls\""):]
        base = base[:base.index("\nfi\n")]
        for name, source in (("pre-merge", premerge), ("base", base)):
            full = [line for line in source.splitlines() if "discover -s scripts/ci -p" in line and "test_*" in line]
            self.assertTrue(full, name)
            for line in full:
                self.assertIn(exclude, line, name)
        phase = gate[gate.index('pool_phase "Verify PID-namespace isolation controls"'):]
        phase = phase[:phase.index("\nfi\n")]
        self.assertIn("OKILUM_PIDNS_CONTROLS=1", phase)
        self.assertIn("test_pid_namespace", phase)
        self.assertRegex(gate, r'if has admission; then\n(    #.*\n)*    pool_phase "Verify PID-namespace isolation controls"')
        self.assertEqual(gate.count("test_pid_namespace"), 2)  # the base exclusion and this phase

        def ids(env):
            with unittest.mock.patch.dict(os.environ, env):
                suite = unittest.defaultTestLoader.discover(str(ROOT / "scripts/ci"), pattern="test_pid_namespace.py")
            found = []

            def walk(item):
                if isinstance(item, unittest.TestSuite):
                    for child in item:
                        walk(child)
                else:
                    found.append(item.id())
            walk(suite)
            return found
        # The exclusion removes them even where they would otherwise be mandatory.
        present = ids({"OKILUM_PIDNS_CONTROLS": "1", "OKILUM_UNITTEST_EXCLUDE": ""})
        self.assertEqual(len([i for i in present if ".PidNamespaceControls." in i]), 3, present)
        self.assertEqual(ids({"OKILUM_PIDNS_CONTROLS": "1", "OKILUM_UNITTEST_EXCLUDE": "test_pid_namespace"}), [])



# The real shard branching (pool-linux-gate.sh) and phase library, with every
# phase command replaced by `true`, or `false` for the injected ones.
STUB_LIB = r'''
eval "$(declare -f pool_phase | sed '1s/^pool_phase /pool_phase_real /')"
pool_phase() {
    local name=$1
    if [[ ",${POOL_TEST_FAIL:-}," == *",${OKILUM_POOL_SHARD}:${name},"* ]]; then
        pool_phase_real "$name" false
    else
        pool_phase_real "$name" true
    fi
}
pool_ensure_git() { :; }
pool_require_pidns() { :; }
'''


class PoolGateEndToEnd(unittest.TestCase):
    """Failure injection through shard branching, aggregation and every workflow reporter."""

    def shards(self):
        dispatch = (ROOT / "scripts/ci/pool-dispatch.sh").read_text()
        block = dispatch[dispatch.index("    linux-full)"):]
        block = block[:block.index(";;")]
        return re.findall(r'^\s+"([a-z-]+) \d+ ', block, re.M)

    def reporters(self):
        workflow = yaml.safe_load((ROOT / ".github/workflows/agent-checks.yml").read_text())
        steps = []
        for step in workflow["jobs"]["linux"]["steps"]:
            run, condition = step.get("run", ""), str(step.get("if", ""))
            # An ordinary PR run: no benchmark-only or publication-only steps.
            benchmark_only = "agent-checks-benchmark/" in condition and "!startsWith" not in condition
            if "pool-phase.sh" not in run or benchmark_only or "publication_baseline" in condition:
                continue
            steps.append((step["name"], run))
        return steps

    def gate(self, fail=()):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts/ci").mkdir(parents=True)
            shutil.copy(ROOT / "scripts/ci/pool-linux-gate.sh", root / "scripts/ci/pool-linux-gate.sh")
            lib = (ROOT / "scripts/ci/pool-phase-lib.sh").read_text() + STUB_LIB
            (root / "scripts/ci/pool-phase-lib.sh").write_text(lib)
            ship = root / "ship"
            dirs = []
            for shard in self.shards():
                out = ship / shard
                out.mkdir(parents=True)
                env = {**os.environ, "OKILUM_POOL_RECEIPT": str(out / "phases.json"),
                       "POOL_TEST_FAIL": ",".join(fail), "TMPDIR": str(root)}
                env.pop("OKILUM_PUBLICATION_BASELINE", None)
                env.pop("OKILUM_POOL_BENCHMARK", None)
                result = subprocess.run(["bash", "scripts/ci/pool-linux-gate.sh", shard], cwd=root, env=env,
                                        capture_output=True, text=True, timeout=120)
                (out / "rc").write_text(f"{result.returncode}\n")
                (out / "build-slot.json").write_text(json.dumps(
                    {"host": "baldr", "exit": result.returncode, "oom_kill": 0}))
                dirs.append(str(out))
            receipt, states = ship / "phases.json", ship / "shards.json"
            summary = subprocess.run([sys.executable, str(ROOT / "scripts/ci/pool-aggregate.py"),
                                      "--out", str(receipt), "--shards-out", str(states), *dirs],
                                     capture_output=True, text=True, timeout=60, check=True).stdout
            kind = re.search(r"^kind=(\w+)$", summary, re.M).group(1)
            results = {}
            env = {"OKILUM_POOL_RECEIPT": str(receipt), "OKILUM_POOL_SHARDS": str(states),
                   "PATH": os.environ["PATH"]}
            for name, run in self.reporters():
                results[name] = subprocess.run(["bash", "-c", run], cwd=ROOT, env=env, capture_output=True,
                                               text=True, timeout=60).returncode
            return kind, results

    def test_all_shards_green_passes_every_reporter(self):
        self.assertEqual(self.shards(), ["base", "rust-a", "rust-b", "admission", "sync", "desktop"])
        kind, results = self.gate()
        self.assertEqual(kind, "verdict")
        self.assertIn("Verify every pool shard and phase passed", results)
        self.assertEqual({name for name, rc in results.items() if rc}, set())

    def test_every_injected_shard_failure_fails_the_gate(self):
        overall = "Verify every pool shard and phase passed"
        for shard, phase, also in (
                ("sync", "Verify local writer authority and fencing", "Verify local writer authority and fencing"),
                ("base", "Verify bundled Sync Help and brand", "Verify bundled Sync Help and brand"),
                # The user bus fails before rust-b's census: rust-a's passing
                # record of the same phase must not satisfy its reporter.
                ("rust-b", "Start a systemd user manager for updater unit controls",
                 "Run Linux Rust suites offline"),
                ("desktop", "Pair through a running okilum sync serve", "Pair through a running okilum sync serve"),
                # A setup phase without its own reporter is caught by the overall check.
                ("admission", "Fetch locked public dependencies", None)):
            with self.subTest(shard=shard, phase=phase):
                kind, results = self.gate([f"{shard}:{phase}"])
                self.assertEqual(kind, "verdict")
                self.assertEqual(results[overall], 1)
                if also:
                    self.assertEqual(results[also], 1, also)

    def test_every_phase_of_every_shard_is_reported_or_covered_by_the_overall_check(self):
        gate = (ROOT / "scripts/ci/pool-linux-gate.sh").read_text()
        phases = set(re.findall(r'pool_phase "([^"]+)"', gate))
        reported = {name for name, _ in self.reporters()}
        workflow = (ROOT / ".github/workflows/agent-checks.yml").read_text()
        # Setup phases: a failure there is reported by the overall check only.
        setup = {"Verify pinned Rust toolchain", "Fetch locked public dependencies",
                 "Prepare isolated Python test dependencies"}
        conditional = {"Warm build Linux desktop offline", "Warm complete okilum-core suite offline",
                       "Warm Linux Rust suites offline", "Compare editor publication with pinned main on this host"}
        for phase in phases - setup:
            if phase in conditional:
                self.assertIn(f'pool-phase.sh "{phase}"', workflow, phase)
            else:
                self.assertIn(phase, reported, phase)


if __name__ == "__main__":
    unittest.main()
