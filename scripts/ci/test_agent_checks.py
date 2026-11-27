"""Controls for source-pinned hosted worker verdicts (no compiler or network)."""
import copy
import importlib.util
import contextlib
import io
import os
import re
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
import yaml


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("gh_check", ROOT / "scripts/agent/gh_check.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
workspace_spec = importlib.util.spec_from_file_location(
    "workspace_tests", ROOT / "scripts/ci/workspace-rust-tests.py")
workspace = importlib.util.module_from_spec(workspace_spec)
workspace_spec.loader.exec_module(workspace)
target_spec = importlib.util.spec_from_file_location(
    'target_tests', ROOT / 'scripts/ci/require-target-tests.py')
target_tests = importlib.util.module_from_spec(target_spec)
target_spec.loader.exec_module(target_tests)
SHA = "a" * 40


def receipt():
    return {"headSha": SHA, "status": "completed", "conclusion": "success", "jobs": [
        {"name": "linux", "conclusion": "success", "steps": [
            {"name": n, "conclusion": "success"} for n in (
                "Prepare and verify pinned vendor and comrak", "Lint every workspace target offline", "Build Linux desktop offline",
                "Run complete okilum-core suite offline", "Run Linux Rust suites offline",
                "Verify agent helper controls", "Stress private reveal bus (20 runs)",
                "Run CI, releases and updater Python suites", "Verify per-user AppImage runtime",
                "Verify pinned Sync fixture lifecycle, source sharing and remote inspection",
                "Verify managed launch and configuration admission",
                "Verify real disk-full Sync transfer and first merge",
                "Reject and reap surviving agent fixture daemons", "Verify additive vault regression controls")]},
        {"name": "windows-recovery", "conclusion": "success", "steps": [
            {"name": n, "conclusion": "success"} for n in (
                "Prepare and verify Windows vendor", "Run native Windows recovery controls",
                "Run native Windows delta console controls", "Run native Windows Sync identity controls",
                "Run native Windows atlas upload controls",
                "Run native Windows terminal console controls",
                "Run native Windows Kubernetes transport controls",
                "Run native Windows okilum-cored CLI and MCP suites")]}]}



MAIN_REF = "github.ref == 'refs/heads/main'"


def _top_level(expression, operator):
    """Split on operator outside parentheses and quotes."""
    parts, depth, quote, start, i = [], 0, False, 0, 0
    while i < len(expression):
        char = expression[i]
        if char == "'":
            quote = not quote
        elif not quote and char == "(":
            depth += 1
        elif not quote and char == ")":
            depth -= 1
        elif not quote and depth == 0 and expression.startswith(operator, i):
            parts.append(expression[start:i])
            start = i + len(operator)
            i = start
            continue
        i += 1
    return parts + [expression[start:]]


def restricts_to_main(guard):
    """True when the expression is a conjunction with the exact main-ref check."""
    expression = str(guard or "").strip()
    if expression.startswith("${{") and expression.endswith("}}"):
        expression = expression[3:-2].strip()
    if len(_top_level(expression, "||")) != 1:
        return False
    return any(" ".join(part.split()) == MAIN_REF for part in _top_level(expression, "&&"))


def duplicate_mapping_keys(text, name):
    """Return "name:line key" for every key repeated inside one YAML mapping."""
    found = []

    class Loader(yaml.SafeLoader):
        pass

    def construct(loader, node, deep=False):
        seen = set()
        for key_node, _ in node.value:
            key = loader.construct_object(key_node, deep=deep)
            if key in seen:
                found.append(f"{name}:{key_node.start_mark.line + 1} {key}")
            seen.add(key)
        return yaml.SafeLoader.construct_mapping(loader, node, deep)

    Loader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, construct)
    yaml.load(text, Loader=Loader)
    return found


def cache_save_violations(workflows):
    """(cache actions found, unguarded saves) over {file name: parsed workflow}."""
    found, violations = 0, []
    for file, workflow in sorted(workflows.items()):
        for name, job in (workflow or {}).get("jobs", {}).items():
            job_guard = restricts_to_main(job.get("if"))
            for step in job.get("steps", []):
                action = str(step.get("uses", "")).lower().split("@")[0]
                where = f"{file}:{name}:{step.get('uses')}"
                if action in ("actions/cache", "actions/cache/save"):
                    found += 1
                    if not (job_guard or restricts_to_main(step.get("if"))):
                        violations.append(where)
                elif action == "swatinem/rust-cache":
                    found += 1
                    if not restricts_to_main((step.get("with") or {}).get("save-if")):
                        violations.append(where)
    return found, violations


def release_shape(command, verb):
    """Profile, packages, binaries and features of one cargo invocation."""
    words = command.split()
    assert words[:2] == ["cargo", verb], command
    shape = {"profile": "dev", "-p": set(), "--bin": set(), "features": set(), "other": set()}
    rest = iter(words[2:])
    for word in rest:
        if word == "--release":
            shape["profile"] = "release"
        elif word == "--profile":
            shape["profile"] = next(rest)
        elif word in ("-p", "--bin"):
            shape[word].add(next(rest))
        elif word in ("--features", "--all-features", "--all-targets", "--tests", "--benches", "--examples"):
            shape["features"].add(word if word != "--features" else next(rest))
        elif word not in ("--locked", "--offline"):
            shape["other"].add(word)
    return shape


def appimage_release_shape():
    lines = [line for line in (ROOT / "scripts/appimage/build.sh").read_text().splitlines()
             if line.startswith("cargo build ")]
    assert len(lines) == 1, lines
    # The beta build: release profile, no acceptance-harness features.
    command = lines[0].replace('"$cargo_profile"', "release").replace('"${build_features[@]}"', "")
    return release_shape(command, "build")

class VerdictTests(unittest.TestCase):
    def setUp(self):
        ambient = patch.dict(os.environ, OKILUM_WORKSPACE_ORDINARY_SHARDS='2')
        ambient.start()
        self.addCleanup(ambient.stop)

    def test_heavy_workspace_shards_keep_inventory_overlap_and_failure_verdict(self):
        names = [f'ordinary::case{i}' for i in range(80)]
        names += ['ordinary::case0', 'ordinary::case0::nested', 'reader_log::case',
                  'platform::reveal::tests::case']
        inventory = subprocess.CompletedProcess([], 0, stdout=''.join(
            name + ': test\n' for name in names), stderr='')
        for failed in (None, 'ordinary-3', 'logs', 'reveal'):
            with self.subTest(failed=failed):
                barrier = threading.Barrier(5)
                seen = []
                completed = set()
                lock = threading.Lock()

                def execute(name, arguments, expected):
                    if name.startswith('ordinary-'):
                        self.assertIn('--test-threads=2', arguments)
                        self.assertIn('--exact', arguments)
                        selected = [item for item in names if item in arguments]
                    else:
                        selected = [item for item in names if arguments[0] in item]
                    self.assertEqual(len(selected), expected)
                    if name != 'reveal':
                        barrier.wait(timeout=3)
                    else:
                        self.assertEqual(completed, {f'ordinary-{i}' for i in range(4)} | {'logs'})
                    with lock:
                        seen.extend(selected)
                        completed.add(name)
                    return name != failed

                with patch.dict(os.environ, OKILUM_WORKSPACE_ORDINARY_SHARDS='4'), \
                        patch.object(workspace.subprocess, 'run', return_value=inventory), \
                        patch.object(workspace, 'run', side_effect=execute), \
                        contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(workspace.main(), int(failed is not None))
                self.assertCountEqual(seen, names)

    def test_shared_census_preserves_workspace_selection_and_exclusions(self):
        ordinary = [f'case::{i}' for i in range(80)] + ['case::0', 'case::0::nested']
        names = ordinary + ['tests::appimage']
        inventory = subprocess.CompletedProcess([], 0, stdout=(
            'Running unittests src/lib.rs (target/debug/deps/okilum_core-abcdef)\n'
            + ''.join(name + ': test\n' for name in ordinary)
            + 'Running unittests src/lib.rs (target/debug/deps/okilum_appimage-fedcba)\n'
              'tests::appimage: test\n'), stderr='')
        for failed in (None, 'ordinary-2'):
            seen = []
            barrier = threading.Barrier(5)
            lock = threading.Lock()

            def execute(name, arguments, expected, *, command, limit):
                self.assertEqual(limit, 3600)
                self.assertEqual(command, ['cargo', 'test', '--workspace', '--locked',
                                           '--no-fail-fast', '--'])
                self.assertIn('--test-threads=1' if name == 'appimage'
                              else '--test-threads=2', arguments)
                self.assertIn('--exact', arguments)
                self.assertEqual(arguments[-4:], ['--skip', 'platform::', '--skip', 'reader_log'])
                selected = [item for item in names if item in arguments]
                self.assertEqual(len(selected), expected)
                barrier.wait(timeout=3)
                with lock:
                    seen.extend(selected)
                return name != failed

            with patch.dict(os.environ, OKILUM_WORKSPACE_ORDINARY_SHARDS='4'), \
                    patch.object(workspace.subprocess, 'run', return_value=inventory) as census, \
                    patch.object(workspace, 'run', side_effect=execute), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(workspace.main(shared=True), int(failed is not None))
                self.assertEqual(census.call_args.args[0], [
                    'cargo', 'test', '--workspace', '--locked', '--no-fail-fast', '--',
                    '--list', '--skip', 'platform::', '--skip', 'reader_log'])
            self.assertCountEqual(seen, names)

    def test_shared_census_refuses_unfired_appimage_serial_partition(self):
        inventory = subprocess.CompletedProcess([], 0, stdout=''.join(
            f'case::{i}: test\n' for i in range(80)), stderr='')
        with patch.object(workspace.subprocess, 'run', return_value=inventory), \
                patch.object(workspace, 'run') as execute:
            with self.assertRaisesRegex(AssertionError, 'AppImage serial inventory must fire'):
                workspace.main(shared=True)
        execute.assert_not_called()

    def test_canonical_compile_jobs_left_baldr(self):
        # Okilum left baldr (Oleg 14.11): the canonical shared/Linux gates and the
        # cold-writer diagnostic no longer exist as heavy-runner jobs; the runner
        # label guard (test_runner_labels.py) rejects any baldr label.
        workflow = yaml.safe_load((ROOT / '.forgejo/workflows/ci.yml').read_text())
        self.assertFalse({'shared-local', 'linux-local'} & set(workflow['jobs']))
        self.assertFalse((ROOT / '.forgejo/workflows/cold-writer-diagnostic.yml').exists())

    def test_appimage_fork_cases_are_serial_without_losing_duplicate_names_or_failures(self):
        ordinary = [f'ordinary::case{i}' for i in range(80)] + ['tests::lock::nested']
        serial = ['tests::lock', 'tests::restart', 'tests::lock']
        for shared in (False, True):
            resources = [] if shared else ['reader_log::case', 'platform::reveal::tests::case']
            names = ordinary + serial + resources
            inventory = subprocess.CompletedProcess([], 0, stdout=(
                'Running unittests src/lib.rs (target/debug/deps/okilum_appimage-abcdef)\n'
                'tests::lock: test\ntests::restart: test\n'
                'Running unittests src/lib.rs (target/debug/deps/okilum_core-fedcba)\n'
                + ''.join(name + ': test\n' for name in ordinary + ['tests::lock'] + resources)), stderr='')
            for failed in (None, 'appimage', 'ordinary-1'):
                with self.subTest(shared=shared, failed=failed):
                    barrier = threading.Barrier(5 if shared else 6)
                    seen, completed = [], set()
                    lock = threading.Lock()

                    def execute(name, arguments, expected, **options):
                        if name.startswith('ordinary-') or name == 'appimage':
                            self.assertIn('--exact', arguments)
                            self.assertIn('--test-threads=1' if name == 'appimage'
                                          else '--test-threads=2', arguments)
                            selected = [item for item in names if item in arguments]
                            if name == 'appimage':
                                self.assertCountEqual(selected, serial)
                            else:
                                self.assertFalse(set(selected) & set(serial))
                        else:
                            selected = [item for item in names if arguments[0] in item]
                        self.assertEqual(len(selected), expected)
                        if name != 'reveal':
                            barrier.wait(timeout=3)
                        else:
                            self.assertEqual(completed,
                                             {f'ordinary-{i}' for i in range(4)} | {'appimage', 'logs'})
                        with lock:
                            seen.extend(selected)
                            completed.add(name)
                        return name != failed

                    with patch.dict(os.environ, OKILUM_WORKSPACE_ORDINARY_SHARDS='4'), \
                            patch.object(workspace.subprocess, 'run', return_value=inventory) as census, \
                            patch.object(workspace, 'run', side_effect=execute), \
                            contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(workspace.main(shared=shared), int(failed is not None))
                        self.assertEqual(census.call_args.kwargs['stderr'], subprocess.STDOUT)
                        self.assertEqual(census.call_args.kwargs['stdout'], subprocess.PIPE)
                    self.assertCountEqual(seen, names)

    def test_positive_control_all_required_checks_pass(self):
        self.assertEqual(helper.verdict(receipt(), SHA), 0)
        # Every fast phase needs a successful receipt; hosted `linux` decides
        # the full verdict and does not use fast phase reporters.
        self.assertIn('Verify every pre-merge phase passed', helper.FAST_REQUIRED)
        self.assertEqual(helper.verdict(receipt(), SHA, benchmark=True), 75)
        info = receipt()
        info['jobs'][0]['steps'] += [dict(name=n, conclusion='success') for n in (
            'Warm build Linux desktop offline', 'Warm complete okilum-core suite offline',
            'Warm Linux Rust suites offline')]
        self.assertEqual(helper.verdict(info, SHA, benchmark=True), 0)

    def test_native_mac_sync_is_required_only_when_requested(self):
        data = receipt()
        self.assertEqual(helper.verdict(data, SHA, macos_sync=True), 75)
        mac = dict(name="macos-sync", conclusion="skipped", steps=[])
        data["jobs"].append(mac)
        self.assertEqual(helper.verdict(data, SHA), 0)
        self.assertEqual(helper.verdict(data, SHA, macos_sync=True), 75)
        mac["conclusion"] = "success"
        for step in ("Prepare and verify Mac Sync vendor", "Lint native Mac Sync targets", "Run native Mac Sync controls"):
            self.assertEqual(helper.verdict(data, SHA, macos_sync=True), 75)
            mac["steps"].append(dict(name=step, conclusion="success"))
        self.assertEqual(helper.verdict(data, SHA, macos_sync=True), 0)
        self.assertEqual(helper.verdict(data, "b" * 40, macos_sync=True), 75)
        data["jobs"].append(dict(mac))
        self.assertEqual(helper.verdict(data, SHA, macos_sync=True), 75)

    def test_requested_publication_control_is_a_required_successful_step(self):
        data = receipt()
        self.assertEqual(helper.verdict(data, SHA, publication_controls=True), 75)
        data["jobs"][0]["steps"].append(dict(name="Compare editor publication with pinned main on this host", conclusion="success"))
        self.assertEqual(helper.verdict(data, SHA, publication_controls=True), 0)

    def test_other_head_or_missing_tests_cannot_pass(self):
        self.assertEqual(helper.verdict(receipt(), "b" * 40), 75)
        for index in range(len(receipt()["jobs"][0]["steps"])):
            data = receipt()
            data["jobs"][0]["steps"].pop(index)
            self.assertEqual(helper.verdict(data, SHA), 75)

    def test_failure_vs_infrastructure_and_cancelled(self):
        for name, expected in (("Lint every workspace target offline", 1),
                               ("Run CI, releases and updater Python suites", 1),
                               ("Run Linux Rust suites offline", 1),
                               ("Run complete okilum-core suite offline", 1),
                               ("Prepare and verify pinned vendor and comrak", 1),
                               ("Install system libraries", 75),
                               ("Reclaim unused hosted SDKs for Rust artifacts", 75),
                               ("Dispatch Linux full gate to the build-slot pool", 75),
                               ("Prepare isolated Python test dependencies", 75),
                               ("actions/checkout@v4", 75)):
            data = receipt()
            data["conclusion"] = "failure"
            data["jobs"][0]["conclusion"] = "failure"
            data["jobs"][0]["steps"].append({"name": name, "conclusion": "failure"})
            self.assertEqual(helper.verdict(data, SHA), expected)
        for conclusion in ("cancelled", "timed_out", "startup_failure", "skipped"):
            data = receipt()
            data["conclusion"] = conclusion
            self.assertEqual(helper.verdict(data, SHA), 75)

    def test_watchdog_kill_is_a_test_failure_while_real_resource_failures_are_infra(self):
        timeout = 'TEST_GROUP_TIMEOUT group=workspace-full limit=1320s kill_after=30s\n'
        for killed in ('exit code 137', 'signal: 9, SIGKILL'):
            self.assertTrue(helper.infrastructure_failure(killed))
            self.assertFalse(helper.infrastructure_failure(timeout + killed))
        for real in ('No space left on device', 'Cannot allocate memory',
                     'Could not resolve host', 'Connection timed out'):
            self.assertTrue(helper.infrastructure_failure(timeout + real))

    def pre_merge_receipt(self):
        data = receipt()
        for job in data["jobs"]:
            job["conclusion"] = "skipped"
            job["steps"] = []
        data["jobs"].append({"name": "pre-merge", "conclusion": "success", "steps": [
            {"name": n, "conclusion": "success"} for n in sorted(helper.FAST_REQUIRED)]})
        return data

    def test_pre_merge_gate_accepts_only_its_own_scope(self):
        data = self.pre_merge_receipt()
        self.assertEqual(helper.verdict(data, SHA, fast=True), 0)
        # A pre-merge run is never a full verdict, and a full one is not pre-merge.
        self.assertEqual(helper.verdict(data, SHA), 75)
        self.assertEqual(helper.verdict(receipt(), SHA, fast=True), 75)
        for name in sorted(helper.FAST_REQUIRED):
            with self.subTest(missing=name):
                data = self.pre_merge_receipt()
                data["jobs"][-1]["steps"] = [s for s in data["jobs"][-1]["steps"] if s["name"] != name]
                self.assertEqual(helper.verdict(data, SHA, fast=True), 75)
        ran = self.pre_merge_receipt()
        ran["jobs"][0]["conclusion"] = "success"
        self.assertEqual(helper.verdict(ran, SHA, fast=True), 75, "a compiling job must not run")
        failed = self.pre_merge_receipt()
        failed["conclusion"] = "failure"
        failed["jobs"][-1]["conclusion"] = "failure"
        next(s for s in failed["jobs"][-1]["steps"] if s["name"] == "Check formatting")["conclusion"] = "failure"
        self.assertEqual(helper.verdict(failed, SHA, fast=True), 1)
        self.assertEqual(helper.verdict(self.pre_merge_receipt(), "b" * 40, fast=True), 75)

    def test_pre_merge_job_lints_but_runs_no_rust_tests(self):
        workflow = yaml.safe_load((ROOT / ".github/workflows/agent-checks.yml").read_text())
        on = workflow.get("on", workflow.get(True))
        self.assertIn("agent-checks-fast/**", on["push"]["branches"])
        # No hourly run (#2863). One daily run on main only warms the caches that
        # branch runs restore; branches no longer save their own (#2881).
        crons = [entry["cron"].split() for entry in on.get("schedule", [])]
        self.assertLessEqual(len(crons), 1)
        for minute, hour, day, month, weekday in crons:
            self.assertTrue(minute.isdigit() and hour.isdigit(), "at most one run a day")
        fast = "startsWith(github.ref, 'refs/heads/agent-checks-fast/')"
        jobs = workflow["jobs"]
        self.assertEqual(jobs["pre-merge"]["if"],
                         "${{ " + fast + " || github.event_name == 'schedule' }}")
        for name in ("linux", "windows-recovery"):
            self.assertEqual(jobs[name]["if"], "${{ !" + fast + " }}")
        # The pool Linux gate never runs on a fast or push ref and never fails the run.
        self.assertEqual(jobs["linux-pool"]["if"],
                         "${{ github.event_name == 'workflow_dispatch' && inputs.pool_linux }}")
        self.assertIs(jobs["linux-pool"]["continue-on-error"], True)
        steps = jobs["pre-merge"]["steps"]
        self.assertTrue(helper.FAST_REQUIRED <= {s.get("name") for s in steps})
        self.assertEqual(jobs["pre-merge"]["runs-on"], "ubuntu-24.04")
        # Hosted Ubuntu decides the full verdict until a pool run is green in
        # <= 20 min (Oleg 21.11); the sharded pool gate runs as `linux-pool`.
        self.assertEqual(jobs["linux"]["runs-on"], "ubuntu-24.04")
        self.assertEqual(jobs["linux-pool"]["runs-on"], "okilum-pool")
        self.assertNotIn("pool-dispatch.sh", yaml.dump(jobs["linux"]))
        # The SAME commands run directly on the hosted runner through the
        # shared phase script. No build-slot dispatch is reachable here.
        self.assertNotIn("pool-dispatch.sh", yaml.dump(jobs["pre-merge"]))
        self.assertIn("hosted-pre-merge.py", yaml.dump(jobs["pre-merge"]))
        script = (ROOT / "scripts/ci/pool-pre-merge.sh").read_text()
        self.assertIn("cargo clippy --workspace --all-targets --locked -- -D warnings", script)
        check = next(line.strip() for line in script.splitlines()
                     if line.strip().startswith("cargo check --release"))
        self.assertEqual(release_shape(check, "check"), appimage_release_shape())
        for blob in (script, *[step.get("run", "") for step in steps]):
            for heavy in ("cargo build", "cargo test", "cargo run", "nextest",
                          "at-vault-regression-control", "linux-rust-tests"):
                self.assertNotIn(heavy, blob)
        for job_name in ("linux-pool",):
            dumped = yaml.dump(jobs[job_name])
            self.assertNotIn("actions/cache", dumped)
            self.assertNotIn("hosted-ubuntu-apt", dumped)

    def test_only_main_saves_cargo_caches(self):
        # 10 GB repository quota (#2880, #2881): branch saves evicted main's caches.
        directory = ROOT / ".github/workflows"
        workflows = {path.name: yaml.safe_load(path.read_text())
                     for path in sorted([*directory.glob("*.yml"), *directory.glob("*.yaml")])}
        found, violations = cache_save_violations(workflows)
        self.assertEqual(violations, [])
        self.assertGreaterEqual(found, 20, "cache actions must be discovered")

    def test_cache_save_guard_negative_controls(self):
        main = "${{ github.ref == 'refs/heads/main' }}"
        save = lambda uses, guard: {"w.yml": {"jobs": {"j": {"steps": [
            {"uses": uses, "if": guard, "with": {"path": "target"}}]}}}}
        # Positive control: a guarded save passes.
        self.assertEqual(cache_save_violations(save("actions/cache/save@v4", main)), (1, []))
        # 1. *.yaml files are scanned (the caller globs both; the scanner sees the name).
        found, bad = cache_save_violations({"w.yaml": save("actions/cache/save@v4", None)["w.yml"]})
        self.assertEqual((found, len(bad)), (1, 1))
        # 2. Action names are matched case-insensitively.
        for uses in ("Actions/cache/save@v4", "ACTIONS/CACHE@v4", "swatinem/Rust-Cache@v2"):
            found, bad = cache_save_violations(save(uses, None))
            self.assertEqual((found, len(bad)), (1, 1), uses)
        # 3. Zero discovered cache actions is reported, not silently green.
        self.assertEqual(cache_save_violations({"w.yml": {"jobs": {"j": {"steps": []}}}})[0], 0)
        # 4. The guard must restrict to main: a disjunction or negation does not.
        for guard in ("${{ github.ref == 'refs/heads/main' || github.ref != 'refs/heads/main' }}",
                      "github.ref != 'refs/heads/main'",
                      "${{ !(github.ref == 'refs/heads/main') }}",
                      "${{ always() || github.ref == 'refs/heads/main' }}"):
            self.assertEqual(len(cache_save_violations(save("actions/cache/save@v4", guard))[1]), 1, guard)
        for guard in ("${{ (!cancelled() && steps.c.outputs.cache-hit != 'true') && github.ref == 'refs/heads/main' }}",
                      "success() && github.ref == 'refs/heads/main'"):
            self.assertEqual(cache_save_violations(save("actions/cache/save@v4", guard))[1], [], guard)
        swatinem = {"w.yml": {"jobs": {"j": {"steps": [{"uses": "Swatinem/rust-cache@v2", "with": {
            "save-if": "${{ github.ref == 'refs/heads/main' || true }}"}}]}}}}
        self.assertEqual(len(cache_save_violations(swatinem)[1]), 1)
        # A main-only job guard covers its steps; a disjunctive one does not.
        job = lambda guard: {"w.yml": {"jobs": {"j": {"if": guard, "steps": [
            {"uses": "actions/cache@v4", "with": {"path": "target"}}]}}}}
        self.assertEqual(cache_save_violations(job("github.ref == 'refs/heads/main' && github.run_attempt == 1"))[1], [])
        self.assertEqual(len(cache_save_violations(job("github.ref == 'refs/heads/main' || true"))[1]), 1)

    def test_workflows_have_no_duplicate_mapping_keys(self):
        # GitHub rejects a workflow with a repeated key before any job starts
        # ("workflow file issue"); PyYAML silently keeps the last value. A
        # doubled save-if kept updater-helper-compat red on every push.
        files = sorted(p for d in (".github/workflows", ".forgejo/workflows")
                       for g in ("*.yml", "*.yaml") for p in (ROOT / d).glob(g))
        self.assertGreaterEqual(len(files), 10, "workflows must be discovered")
        found = [dup for path in files for dup in duplicate_mapping_keys(path.read_text(), path.name)]
        self.assertEqual(found, [])

    def test_duplicate_mapping_key_guard_controls(self):
        clean = "jobs:\n  a:\n    steps:\n      - with:\n          save-if: x\n          other: y\n"
        self.assertEqual(duplicate_mapping_keys(clean, "w.yml"), [])
        # Same key in sibling mappings is fine; same key in one mapping is not.
        siblings = "a:\n  k: 1\nb:\n  k: 2\n"
        self.assertEqual(duplicate_mapping_keys(siblings, "w.yml"), [])
        doubled = clean + "          save-if: z\n"
        self.assertEqual(duplicate_mapping_keys(doubled, "w.yml"), ["w.yml:7 save-if"])
        self.assertEqual(len(duplicate_mapping_keys("on: 1\non: 2\n", "w.yml")), 1)

    def test_pool_linux_job_never_decides_a_verdict(self):
        # Oleg 21.11: hosted `linux` gates until a pool run is green in <= 20 min.
        pool_steps = [{"name": "Dispatch Linux full gate to the build-slot pool", "conclusion": "success"},
                      {"name": "Run Linux Rust suites offline", "conclusion": "failure"},
                      {"name": "Verify every pool shard and phase passed", "conclusion": "failure"}]
        for conclusion in ("failure", "success", "skipped", "cancelled"):
            data = receipt()
            data["jobs"].append({"name": "linux-pool", "conclusion": conclusion,
                                 "steps": copy.deepcopy(pool_steps)})
            self.assertEqual(helper.verdict(data, SHA), 0, conclusion)
        # A hosted code failure stays a code failure; a pool infra step cannot mask it.
        data = receipt()
        data["conclusion"] = "failure"
        data["jobs"][0]["conclusion"] = "failure"
        data["jobs"][0]["steps"].append({"name": "Run Linux Rust suites offline", "conclusion": "failure"})
        data["jobs"].append({"name": "linux-pool", "conclusion": "failure", "steps": [
            {"name": "Dispatch Linux full gate to the build-slot pool", "conclusion": "failure"}]})
        self.assertEqual(helper.verdict(data, SHA), 1)
        # A green pool job does not stand in for a missing hosted one.
        data = receipt()
        data["jobs"][0]["name"] = "linux-pool"
        self.assertEqual(helper.verdict(data, SHA), 75)
        # Duplicate job names are still no verdict.
        data = receipt()
        data["jobs"] += [{"name": "linux-pool", "conclusion": "skipped", "steps": []}] * 2
        self.assertEqual(helper.verdict(data, SHA), 75)
        # The fast gate tolerates the skipped pool job and still refuses a ran one.
        fast = self.pre_merge_receipt()
        fast["jobs"].append({"name": "linux-pool", "conclusion": "skipped", "steps": []})
        self.assertEqual(helper.verdict(fast, SHA, fast=True), 0)
        fast["jobs"][-1]["conclusion"] = "success"
        self.assertEqual(helper.verdict(fast, SHA, fast=True), 75)

    def test_partial_empty_or_skipped_jobs_are_not_success(self):
        for jobs in ([], [{"name": "linux", "conclusion": "skipped", "steps": []}]):
            data = receipt()
            data["jobs"] = copy.deepcopy(jobs)
            self.assertEqual(helper.verdict(data, SHA), 75)

    def test_windows_recovery_is_required_and_native_failures_are_not_infra(self):
        data = receipt()
        data["jobs"].pop()
        self.assertEqual(helper.verdict(data, SHA), 75)
        for conclusion in ("skipped", "failure"):
            data = receipt()
            data["jobs"][1]["steps"][-1]["conclusion"] = conclusion
            self.assertEqual(helper.verdict(data, SHA), 75)
        data["conclusion"] = "failure"
        data["jobs"][1]["conclusion"] = "failure"
        self.assertEqual(helper.verdict(data, SHA), 1)

    def test_windows_atlas_controls_are_required_in_the_routine_gate(self):
        data = receipt()
        self.assertEqual(helper.verdict(data, SHA), 0)
        native = data["jobs"][1]["steps"]
        atlas = next(step for step in native if step["name"] == "Run native Windows atlas upload controls")
        for state in ("skipped", "failure"):
            atlas["conclusion"] = state
            self.assertEqual(helper.verdict(data, SHA), 75)
        native.remove(atlas)
        self.assertEqual(helper.verdict(data, SHA), 75)
        workflow = yaml.safe_load((ROOT / ".github/workflows/agent-checks.yml").read_text())
        step = next(step for step in workflow["jobs"]["windows-recovery"]["steps"]
                    if step.get("name") == "Run native Windows atlas upload controls")
        self.assertNotIn("if", step)
        self.assertIn("python scripts/ci/windows-atlas-upload.py", step["run"])
        self.assertIn("exit $LASTEXITCODE", step["run"])

    def test_windows_terminal_console_controls_are_required_in_the_routine_gate(self):
        data = receipt()
        self.assertEqual(helper.verdict(data, SHA), 0)
        native = data["jobs"][1]["steps"]
        terminal = next(step for step in native if step["name"] == "Run native Windows terminal console controls")
        for state in ("skipped", "failure"):
            terminal["conclusion"] = state
            self.assertEqual(helper.verdict(data, SHA), 75)
        native.remove(terminal)
        self.assertEqual(helper.verdict(data, SHA), 75)
        workflow = yaml.safe_load((ROOT / ".github/workflows/agent-checks.yml").read_text())
        step = next(step for step in workflow["jobs"]["windows-recovery"]["steps"]
                    if step.get("name") == "Run native Windows terminal console controls")
        self.assertNotIn("if", step)
        self.assertIn("python scripts/ci/windows-terminal-console.py", step["run"])

    def test_windows_kubernetes_transport_controls_are_required_in_the_routine_gate(self):
        data = receipt()
        self.assertEqual(helper.verdict(data, SHA), 0)
        native = data["jobs"][1]["steps"]
        step = next(s for s in native if s["name"] == "Run native Windows Kubernetes transport controls")
        for state in ("skipped", "failure"):
            step["conclusion"] = state
            self.assertEqual(helper.verdict(data, SHA), 75)
        native.remove(step)
        self.assertEqual(helper.verdict(data, SHA), 75)
        workflow = yaml.safe_load((ROOT / ".github/workflows/agent-checks.yml").read_text())
        step = next(step for step in workflow["jobs"]["windows-recovery"]["steps"]
                    if step.get("name") == "Run native Windows Kubernetes transport controls")
        self.assertNotIn("if", step)
        self.assertIn("windows_job_ends_a_grandchild_holding_the_pipes", step["run"])
        self.assertIn("exit $LASTEXITCODE", step["run"])

    def test_windows_cored_cli_and_mcp_suites_are_required_in_the_routine_gate(self):
        data = receipt()
        self.assertEqual(helper.verdict(data, SHA), 0)
        native = data["jobs"][1]["steps"]
        step = next(s for s in native if s["name"] == "Run native Windows okilum-cored CLI and MCP suites")
        for state in ("skipped", "failure"):
            step["conclusion"] = state
            self.assertEqual(helper.verdict(data, SHA), 75)
        native.remove(step)
        self.assertEqual(helper.verdict(data, SHA), 75)
        workflow = yaml.safe_load((ROOT / ".github/workflows/agent-checks.yml").read_text())
        steps = workflow["jobs"]["windows-recovery"]["steps"]
        step = next(step for step in steps
                    if step.get("name") == "Run native Windows okilum-cored CLI and MCP suites")
        self.assertNotIn("if", step)
        self.assertNotIn("continue-on-error", step)
        self.assertIn("python scripts/ci/windows-cored-suites.py", step["run"])
        self.assertIn("exit $LASTEXITCODE", step["run"])
        # The whole windows-recovery job must still fit its own deadline.
        self.assertLess(step["timeout-minutes"], workflow["jobs"]["windows-recovery"]["timeout-minutes"])

    def test_vault_regression_control_must_run_for_normal_worker_checks(self):
        info = receipt()
        for verdict in ("skipped", "failure"):
            info["jobs"][0]["steps"][-1]["conclusion"] = verdict
            self.assertEqual(helper.verdict(info, SHA), 75)

    def test_worker_lane_has_no_secrets_or_publication(self):
        workflow = (ROOT / ".github/workflows/agent-checks.yml").read_text()
        self.assertNotIn("secrets.", workflow)
        self.assertIn("branches: ['agent-checks/**', 'agent-checks-benchmark/**', 'agent-checks-fast/**']", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("contents: read", workflow)
        self.assertIn("cancel-in-progress: true", workflow)
        self.assertIn("runs-on: okilum-pool", workflow)
        self.assertIn("runs-on: ubuntu-24.04", workflow)
        for other in (ROOT / ".github/workflows").glob("*.yml"):
            if other.name != "agent-checks.yml":
                self.assertNotIn("agent-checks/", other.read_text(), other.name)


class HelperFlowTests(unittest.TestCase):
    def setUp(self):
        # These synthetic inventories exercise the two-shard hosted policy;
        # native heavy discovery inherits four shards from the real workflow.
        ambient = patch.dict(os.environ, OKILUM_WORKSPACE_ORDINARY_SHARDS='2')
        ambient.start()
        self.addCleanup(ambient.stop)

    def test_discovery_rejects_another_source_workflow_branch_or_event(self):
        good = dict(id=7, head_sha=SHA, head_branch="agent-checks/topic", event="push",
                    path=".github/workflows/agent-checks.yml")
        variants = [good]
        for field, value in (("head_sha", "b" * 40), ("head_branch", "main"),
                             ("event", "workflow_dispatch"), ("path", "ci.yml")):
            variants.append({**good, field: value})
        with patch.object(helper, "gh_json", return_value={"workflow_runs": variants}):
            self.assertEqual(helper.find_run("agent-checks/topic", SHA, "push"), [good])

    def git_output(self, *args):
        if args[1:] == ("status", "--porcelain"):
            return ""
        if args[1:] == ("rev-parse", "HEAD"):
            return SHA
        if "symbolic-ref" in args:
            return "topic"
        if "ls-remote" in args:
            return ""
        raise AssertionError(args)

    def test_rerun_waits_for_new_attempt_before_watch_and_verdict(self):
        info = {**receipt(), "attempt": 2, "url": "https://github.com/run/7"}
        replies = [{"attempt": 1}, {"attempt": 1, "status": "completed"},
                   {"attempt": 2, "status": "queued"}, info]
        with patch.object(helper.sys, "argv", ["gh-check", "--rerun"]), \
                patch.object(helper, "output", side_effect=self.git_output), \
                patch.object(helper, "run") as run, \
                patch.object(helper, "find_run", return_value=[{"id": 7, "status": "completed"}]), \
                patch.object(helper, "gh_json", side_effect=replies) as gh_json, \
                patch.object(helper.time, "sleep") as sleep, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(helper.main(), 0)
            self.assertEqual(gh_json.call_count, 4)
            sleep.assert_called_once()
            self.assertTrue(any(c.args[:3] == ("gh", "run", "watch") for c in run.call_args_list))
            push = next(c.args for c in run.call_args_list if "push" in c.args)
            self.assertIn("--force-with-lease=refs/heads/agent-checks/topic:", push)
            self.assertIn(SHA + ":refs/heads/agent-checks/topic", push)

    def test_failed_log_classification_reads_only_gating_jobs(self):
        # #3207 review: a non-gating linux-pool disk-full log must not turn a
        # hosted code failure into infrastructure; a hosted disk-full still does.
        assertion = "linux\tRun Linux Rust suites offline\tassertion failed: left == right\n"
        disk_full = "No space left on device\n"

        def failed_run(hosted_log, pool_log):
            info = {**receipt(), "attempt": 1, "url": "https://github.com/run/7", "conclusion": "failure"}
            for index, job in enumerate(info["jobs"], start=100):
                job["databaseId"] = index
            linux = info["jobs"][0]
            linux["conclusion"] = "failure"
            linux["steps"].append({"name": "Run Linux Rust suites offline", "conclusion": "failure"})
            info["jobs"].append({"name": "linux-pool", "conclusion": "failure", "databaseId": 999, "steps": [
                {"name": "Dispatch Linux full gate to the build-slot pool", "conclusion": "failure"}]})
            logs = {"100": hosted_log, "999": pool_log}
            calls = []

            def logs_for(args, **kwargs):
                self.assertIn("--log-failed", args)
                job = args[args.index("--job") + 1]
                calls.append(job)
                return subprocess.CompletedProcess(args, 0, stdout=logs[job])

            with patch.object(helper.sys, "argv", ["gh-check"]), \
                    patch.object(helper, "output", side_effect=self.git_output), \
                    patch.object(helper, "run"), \
                    patch.object(helper, "find_run", return_value=[{"id": 7, "status": "completed"}]), \
                    patch.object(helper, "gh_json", return_value=info), \
                    patch.object(helper.subprocess, "run", side_effect=logs_for), \
                    contextlib.redirect_stdout(io.StringIO()):
                return helper.main(), calls

        self.assertEqual(failed_run(assertion, ""), (1, ["100"]))
        self.assertEqual(failed_run(assertion, disk_full), (1, ["100"]), "pool log must not reclassify")
        self.assertEqual(failed_run(assertion + disk_full, ""), (75, ["100"]), "hosted disk-full is infra")

    def test_dirty_worktree_never_pushes_or_reuses_a_green_result(self):
        with patch.object(helper.sys, "argv", ["gh-check"]), \
                patch.object(helper, "output", return_value=" M source.rs"), \
                patch.object(helper, "run") as run, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(helper.main(), 75)
            self.assertEqual(run.call_count, 1)  # repository existence only

    def test_linux_suite_extraction_preserves_existing_acceptance_commands(self):
        linux = (ROOT / "scripts/ci/check-linux.sh").read_text()
        suites = (ROOT / "scripts/ci/linux-rust-tests.sh").read_text()
        self.assertIn("python3 -m unittest discover -s scripts/ci", linux)
        self.assertIn("bash scripts/ci/linux-rust-tests.sh", linux)
        for command in ("cargo test -p okilum-appimage --all-features",
                        "bash scripts/ci/vendor-tests.sh",
                        "cargo test -p okilum-core --no-default-features --test portable_reader"):
            self.assertIn(command, suites)
        self.assertIn("suite workspace-full 1320 python3 scripts/ci/workspace-rust-tests.py", suites)
        self.assertEqual(workspace.COMMAND[:3], ["cargo", "test", "--locked"])
        self.assertEqual([workspace.COMMAND[i + 1] for i, arg in enumerate(workspace.COMMAND)
                          if arg == "-p"],
                         ["okilum-core", "okilum-shell", "okilum-shell-kit", "okilum-shell-platform", "okilum-shell-text", "okilum-shell-viewers", "okilum-shell-logs", "okilum-shell-sync", "okilum-shell-project", "okilum-shell-brain", "okilum-sync", "okilum-t3"])
        self.assertEqual(workspace.COMMAND[-2:], ["--no-fail-fast", "--"])

    def test_workspace_census_rejects_missing_failed_and_unfired_controls(self):
        # Real harness shapes, including a child verdict before its parent.
        lines = ["Running unittests src/lib.rs (fixture)\n",
                 "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 998 filtered out;\n",
                 "test result: ok. 982 passed; 0 failed; 17 ignored; 0 measured; 0 filtered out;\n",
                 "Running tests/fixture.rs (fixture)\n",
                 "test result: ok. 6 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out;\n"]
        for text, status, expected, accepted in [
            (lines, 0, 1006, True),
            (lines[:2] + lines[3:], 0, 1006, False),  # missing parent verdict
            (lines, 0, 1007, False),  # missing case
            (lines, 1, 1006, False),  # real command failure
            (["test result: ok. 0 passed; 0 failed; 1 ignored;\n"], 0, 1, False),
        ]:
            process = type("Process", (), {"stdout": iter(text), "wait": lambda self: status})()
            with patch.object(workspace.subprocess, "Popen", return_value=process), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(workspace.run("fixture", [], expected), accepted)

    def test_workspace_log_and_ordinary_overlap_but_reveal_waits_and_failures_survive(self):
        inventory = subprocess.CompletedProcess([], 0, stdout=(
            "ordinary::case0: test\nordinary::case2: test\nreader_log::case: test\n"
            "platform::reveal::tests::case: test\n"), stderr="")
        for failed in (None, "ordinary-0", "ordinary-1", "logs", "reveal"):
            with self.subTest(failed=failed):
                barrier = threading.Barrier(3)
                completed = set()
                lock = threading.Lock()

                def execute(name, arguments, expected):
                    self.assertEqual(expected, 1)
                    if name in ("ordinary-0", "ordinary-1", "logs"):
                        # A serial regression cannot satisfy this positive
                        # rendezvous; no compiler, services or sleeps involved.
                        barrier.wait(timeout=3)
                    else:
                        with lock:
                            self.assertEqual(completed, {"ordinary-0", "ordinary-1", "logs"})
                    with lock:
                        completed.add(name)
                    return name != failed

                with patch.object(workspace.subprocess, "run", return_value=inventory), \
                        patch.object(workspace, "run", side_effect=execute), \
                        contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(workspace.main(), int(failed is not None))
                self.assertEqual(completed, {"ordinary-0", "ordinary-1", "logs", "reveal"})

    def test_workspace_exact_shards_preserve_duplicate_and_prefix_names_once(self):
        names = ['ordinary::case0', 'ordinary::case2', 'ordinary::case0',
                 'ordinary::case0::nested', 'reader_log::case',
                 'platform::reveal::tests::case']
        inventory = subprocess.CompletedProcess([], 0, stdout=''.join(
            name + ': test\n' for name in names), stderr='')
        seen = []
        threads = []
        lock = threading.Lock()

        def execute(name, arguments, expected):
            if name.startswith('ordinary-'):
                self.assertIn('--exact', arguments)
                self.assertIn('--test-threads=2', arguments)
                selected = [item for item in names if item in arguments]
                threads.append(2)
            else:
                selected = [item for item in names if arguments[0] in item]
            self.assertEqual(len(selected), expected)
            with lock:
                seen.extend(selected)
            return True

        with patch.object(workspace.subprocess, 'run', return_value=inventory), \
                patch.object(workspace, 'run', side_effect=execute), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(workspace.main(), 0)
        self.assertCountEqual(seen, names)
        self.assertEqual(sum(threads), 4)

    @staticmethod
    def fake_linux_suite_root(directory):
        """Copy the real Linux suite list beside recording stubs; returns (root, scripts, fake)."""
        root = Path(directory)
        scripts = root / 'scripts/ci'
        scripts.mkdir(parents=True)
        for name in ('linux-rust-tests.sh', 'test-nofile.sh', 'test-group.sh', 'test-group.py',
                     'workspace-rust-tests.py'):
            shutil.copy2(ROOT / 'scripts/ci' / name, scripts / name)
        # This control measures command inventory and first-failure retention,
        # not process supervision. Record every wrapper invocation and run
        # each fake command directly; the next control runs the real watchdog.
        (scripts / 'test-group.sh').write_text(
            'if [[ ${1:-} == --isolated-writer-account ]]; then\n'
            '  shift; printf "%s\\n" "$1" >> "$ACCOUNT_LOG"\nfi\n'
            'printf "%s %s\\n" "$1" "$2" >> "$GROUP_LOG"\n'
            'shift 2\nexec "$@"\n')
        (scripts / 'vendor-tests.sh').write_text('echo vendor-tests >> "$CALL_LOG"\n')
        (scripts / 'local-writer-admission.sh').write_text(
            'echo local-writer-admission >> "$CALL_LOG"\n')
        fake = root / 'bin'
        fake.mkdir()
        cargo = fake / 'cargo'
        cargo.write_text('#!/usr/bin/env bash\n'
                         'echo "$* sprint=${OKILUM_SPRINT_BUILD:-0}" >> "$CALL_LOG"\n'
                         'if [[ "$FAIL_FIRST" = 1 && ! -e "$FAIL_MARKER" ]]; then\n'
                         '  touch "$FAIL_MARKER"; exit 7\n'
                         'fi\n'
                         'if [[ " $* " == *" -p okilum-core -p okilum-shell -p okilum-shell-kit -p okilum-shell-platform -p okilum-shell-text -p okilum-shell-viewers -p okilum-shell-logs -p okilum-shell-sync -p okilum-shell-project -p okilum-shell-brain -p okilum-sync -p okilum-t3 "* ]]; then\n'
                         '  if [[ " $* " == *" --list "* ]]; then\n'
                         '    printf "ordinary::case0: test\\nordinary::case2: test\\nreader_log::sample: test\\nplatform::reveal::tests::sample: test\\n"\n'
                         '  else\n'
                         '    printf "Running unittests src/lib.rs (stub)\\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\\n"\n'
                         '  fi\n'
                         'fi\n')
        cargo.chmod(0o755)
        # The shared suite also runs web reachability checks. Stub every
        # test runner so this failure-propagation control stays isolated.
        shutil.copy2(cargo, fake / 'node')
        return root, scripts, fake

    def test_linux_suites_keep_the_first_failure_and_still_execute_later_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            root, scripts, fake = self.fake_linux_suite_root(directory)
            counts = []
            for fail in ('0', '1'):
                log = root / ('calls-' + fail)
                groups = root / ('groups-' + fail)
                accounts = root / ('accounts-' + fail)
                result = subprocess.run(['bash', str(scripts / 'linux-rust-tests.sh')], cwd=root,
                                        env={**os.environ, 'PATH': str(fake) + os.pathsep + os.environ['PATH'],
                                             'CALL_LOG': str(log), 'GROUP_LOG': str(groups), 'FAIL_FIRST': fail,
                                             'ACCOUNT_LOG': str(accounts),
                                             'FAIL_MARKER': str(root / 'failed')},
                                        capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, int(fail), result.stdout + result.stderr)
                calls = log.read_text().splitlines()
                self.assertCountEqual(accounts.read_text().splitlines(),
                                      ['reader-editor-live-preview', 'issue2486',
                                       'file-editor-network-save-tests', 'issue1588'])
                self.assertGreater(len(calls), 20)  # positive control: the full list fired
                for selection in ('direct::existing::tests::', 'reader_settings_sync::',
                                  'reader_feedback::ui_tests::feedback2749_',
                                  'reader_editor::live_preview::', 'issue2486_'):
                    selected = [call for call in calls if selection in call]
                    self.assertEqual(len(selected), 1, selection)
                    for package in ('okilum-write-authority', 'okilum-core',
                                    'okilum-shell', 'okilum-t3'):
                        self.assertIn('-p ' + package, selected[0])
                # Separate feature/no-default policies retain their own roots.
                image = next(call for call in calls if '--all-features' in call)
                portable = next(call for call in calls if '--test portable_reader' in call)
                self.assertNotIn('-p okilum-shell', image)
                self.assertNotIn('-p okilum-shell', portable)
                self.assertTrue(any('sprint=1' in call for call in calls))
                self.assertTrue(any('retired-sync.test.js' in call for call in calls))
                self.assertIn('vendor-tests', calls)
                self.assertIn('sidecar::macos::preparation::', calls[-1])
                wrapped = groups.read_text().splitlines()
                workspace_calls = [call for call in calls if
                                   '-p okilum-core -p okilum-shell -p okilum-shell-kit -p okilum-shell-platform -p okilum-shell-text -p okilum-shell-viewers -p okilum-shell-logs -p okilum-shell-sync -p okilum-shell-project -p okilum-shell-brain -p okilum-sync -p okilum-t3' in call]
                self.assertEqual(len(workspace_calls), 5)  # census + four disjoint runs
                self.assertIn('--list', workspace_calls[0])
                ordinary_calls = [call for call in workspace_calls if '--exact' in call]
                self.assertEqual(len(ordinary_calls), 2)
                self.assertTrue(all('--test-threads=2' in call for call in ordinary_calls))
                self.assertTrue(any('ordinary::case0' in call for call in ordinary_calls))
                self.assertTrue(any('ordinary::case2' in call for call in ordinary_calls))
                self.assertTrue(any('reader_log --nocapture --test-threads=1' in call
                                    for call in workspace_calls[1:4]))
                self.assertIn('platform::reveal::tests:: --nocapture --test-threads=1', workspace_calls[4])
                self.assertEqual(len(wrapped) + 4, len(calls))
                self.assertIn('issue2049 120', wrapped)
                self.assertEqual(wrapped[-1], 'sidecar-macos-preparation 120')
                counts.append(len(calls))
            self.assertEqual(counts[0], counts[1])

    def test_linux_suite_parts_split_the_full_list_into_two_disjoint_halves(self):
        # #3198: the daily hosted gate runs `checks` and `workspace` as parallel
        # jobs. Together they must run exactly the `all` groups, each once.
        with tempfile.TemporaryDirectory() as directory:
            root, scripts, fake = self.fake_linux_suite_root(directory)
            def run(*part):
                log, groups = root / ('calls-' + '-'.join(part or ('none',))), root / ('groups-' + '-'.join(part or ('none',)))
                result = subprocess.run(['bash', str(scripts / 'linux-rust-tests.sh'), *part], cwd=root,
                                        env={**os.environ, 'PATH': str(fake) + os.pathsep + os.environ['PATH'],
                                             'CALL_LOG': str(log), 'GROUP_LOG': str(groups), 'FAIL_FIRST': '0',
                                             'ACCOUNT_LOG': str(root / 'accounts'),
                                             'FAIL_MARKER': str(root / 'failed')},
                                        capture_output=True, text=True, timeout=30)
                read = lambda path: path.read_text().splitlines() if path.exists() else []
                return result, read(log), [line.split()[0] for line in read(groups)]
            result, calls, everything = run()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(run('all')[2], everything)
            self.assertIn('workspace-full', everything)  # positive control
            result, checks_calls, checks = run('checks')
            self.assertEqual(result.returncode, 0, result.stderr)
            result, workspace_calls, workspace = run('workspace')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn('workspace-full', checks)
            self.assertEqual([group for group in workspace if not group.startswith('workspace-')],
                             [])
            self.assertEqual([group for group in everything if group != 'workspace-full'
                              and not group.startswith('workspace-')], checks)
            self.assertEqual([group for group in everything if group.startswith('workspace-')], workspace)
            # The census selection is compiled before the census deadline starts.
            self.assertIn('--no-run', workspace_calls[0])
            self.assertIn('-p okilum-core -p okilum-shell -p okilum-shell-kit -p okilum-shell-platform -p okilum-shell-text -p okilum-shell-viewers -p okilum-shell-logs -p okilum-shell-sync -p okilum-shell-project -p okilum-shell-brain -p okilum-sync -p okilum-t3', workspace_calls[0])
            self.assertFalse(any('--no-run' in call for call in calls + checks_calls))
            result, calls, groups = run('bogus')
            self.assertEqual((result.returncode, calls, groups), (2, [], []))
            self.assertIn('unknown part', result.stderr)

    def test_native_frame_measurement_waits_for_ordinary_cases_and_keeps_its_failure(self):
        for shared in (False, True):
            first_content = 'reader_code_file::review_tests::code2809_first_text_precedes_full_parse_and_format_on_three_point_five_mb'
            names = ['ordinary::case0', 'ordinary::case2', workspace.NATIVE_FRAME, first_content]
            resources = ['tests::appimage'] if shared else [
                'reader_log::case', 'platform::reveal::tests::case']
            names += resources
            inventory = subprocess.CompletedProcess([], 0, stdout=(
                'Running unittests src/main.rs (target/debug/deps/okilum-abcdef)\n'
                + ''.join(name + ': test\n' for name in names if name != 'tests::appimage')
                + ('Running unittests src/lib.rs (target/debug/deps/okilum_appimage-abcdef)\n'
                   'tests::appimage: test\n' if shared else '')), stderr='')
            for failed in (None, 'native-frame', 'first-content', 'ordinary-0'):
                with self.subTest(shared=shared, failed=failed):
                    completed, seen = set(), []
                    lock = threading.Lock()
                    barrier = threading.Barrier(3)

                    def execute(name, arguments, expected, **options):
                        if name in ('first-content', 'native-frame', 'reveal'):
                            earlier = {'ordinary-0', 'ordinary-1'} | ({'appimage'} if shared else {'logs'})
                            if name != 'first-content':
                                earlier.add('first-content')
                            if name == 'reveal':
                                earlier.add('native-frame')
                            self.assertEqual(completed, earlier)
                        else:
                            barrier.wait(timeout=3)
                        if name.startswith('ordinary-') or name in ('first-content', 'native-frame', 'appimage'):
                            self.assertIn('--exact', arguments)
                            selected = [item for item in names if item in arguments]
                            if name in ('native-frame', 'first-content'):
                                self.assertIn('--test-threads=1', arguments)
                                self.assertEqual(selected, [workspace.NATIVE_FRAME if name == 'native-frame' else first_content])
                            elif name.startswith('ordinary-'):
                                self.assertNotIn(workspace.NATIVE_FRAME, arguments)
                                self.assertNotIn(first_content, arguments)
                        else:
                            selected = [item for item in names if arguments[0] in item]
                        self.assertEqual(len(selected), expected)
                        with lock:
                            seen.extend(selected)
                            completed.add(name)
                        return name != failed

                    with patch.object(workspace.subprocess, 'run', return_value=inventory), \
                            patch.object(workspace, 'run', side_effect=execute), \
                            contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(workspace.main(shared=shared), int(failed is not None))
                    self.assertCountEqual(seen, names)


    @unittest.skipUnless(sys.platform == 'linux', 'Linux process supervision')
    def test_linux_suite_hang_fails_named_group_and_keeps_later_suites(self):
        # Run the real shared entrypoint and supervisor. Only fixture deadlines,
        # TERM grace and runners change; production retains its 30-second grace.
        # A missing wrapper must leave this control hanging/failing.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / 'scripts/ci'
            scripts.mkdir(parents=True)
            for name in ('linux-rust-tests.sh', 'test-nofile.sh', 'test-group.sh', 'test-group.py'):
                shutil.copy2(ROOT / 'scripts/ci' / name, scripts / name)
            entrypoint = scripts / 'linux-rust-tests.sh'
            entrypoint.write_text(entrypoint.read_text()
                .replace('suite issue2049 120 ', 'suite issue2049 2 ')
                .replace('bash scripts/ci/test-group.sh ',
                         'bash scripts/ci/test-group.sh --kill-after 0.3 ', 1))
            for name in ('vendor-tests.sh', 'local-writer-admission.sh'):
                (scripts / name).write_text('exit 0\n')
            fixture = ROOT / 'scripts/ci/fixtures/hung-test-group.py'
            fake = root / 'bin'
            fake.mkdir()
            runner = fake / 'cargo'
            runner.write_text('#!/usr/bin/env bash\n'
                              'if [[ " $* " == *" issue2049_ "* ]]; then\n'
                              '  exec python3 "$HANG_FIXTURE" "$HANG_WORK" test\n'
                              'fi\n'
                              'echo "$*" >> "$CALL_LOG"\n')
            runner.chmod(0o755)
            shutil.copy2(runner, fake / 'node')
            try:
                result = subprocess.run(['bash', str(entrypoint)], cwd=root,
                    env={**os.environ, 'PATH': str(fake) + os.pathsep + os.environ['PATH'],
                         'HANG_FIXTURE': str(fixture), 'HANG_WORK': str(root),
                         'CALL_LOG': str(root / 'calls')}, capture_output=True, text=True, timeout=90)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn('TEST_GROUP_TIMEOUT group=issue2049 limit=2s kill_after=0.3s', result.stdout)
                elapsed = float(re.search(r'TEST_GROUP_END group=issue2049 elapsed=([0-9.]+)s status=124', result.stdout)[1])
                self.assertLessEqual(elapsed, 3)
                self.assertTrue((root / 'ready').exists(), 'hanging tree did not start')
                for pid in root.glob('*.pid'):
                    self.assertFalse(Path('/proc', pid.read_text()).exists(), pid.name)
                self.assertIn('sidecar::macos::preparation::', (root / 'calls').read_text())
            finally:
                # A removed watchdog must fail the control without leaking its
                # TERM-resistant synthetic descendants on the worker host.
                for path in root.glob('*.pid'):
                    try:
                        pid = int(path.read_text())
                        command = Path('/proc', str(pid), 'cmdline').read_bytes()
                        if str(root).encode() in command:
                            os.kill(pid, signal.SIGKILL)
                    except (FileNotFoundError, ProcessLookupError):
                        pass


    def test_local_writer_regression_groups_require_nonzero_passed_counts(self):
        for passed, status, missing in [('1', 0, False), ('0', 0, False), ('1', 7, False), ('1', 0, True)]:
            with self.subTest(passed=passed, status=status, missing=missing), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                scripts = root / 'scripts/ci'
                scripts.mkdir(parents=True)
                for name in ('local-writer-admission.sh', 'test-nofile.sh', 'require-target-tests.py'):
                    shutil.copy2(ROOT / 'scripts/ci' / name, scripts / name)
                fake = root / 'bin'
                fake.mkdir()
                cargo = fake / 'cargo'
                cargo.write_text('#!/usr/bin/env bash\n'
                                 'echo "$*" >> "$CALL_LOG"\n'
                                 'for target in okilum_write_authority okilum_core local_writer_admission okilum okilum_t3; do\n'
                                 '  if [[ "$MISSING" = 1 && "$target" = okilum_write_authority ]]; then continue; fi\n'
                                 '  echo "Running unittests src/lib.rs (target/debug/deps/$target-abc123)"\n'
                                 '  echo "test result: ok. $PASSED passed; 0 failed;"\n'
                                 'done\n'
                                 'exit "$STATUS"\n')
                cargo.chmod(0o755)
                calls = root / 'calls'
                result = subprocess.run(['bash', str(scripts / 'local-writer-admission.sh')],
                    cwd=root, env={**os.environ, 'PATH': str(fake) + os.pathsep + os.environ['PATH'],
                        'CALL_LOG': str(calls), 'PASSED': passed, 'STATUS': str(status), 'MISSING': str(int(missing))},
                    capture_output=True, text=True, timeout=30)
                success = passed == '1' and status == 0 and not missing
                self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
                selection = '-p okilum-write-authority -p okilum-core -p okilum-shell -p okilum-t3'
                expected = [
                    f'test --locked {selection} --lib local_writer_admission -- --nocapture',
                    f'test --locked {selection} --lib file_editor::local_writer_admission_tests -- --nocapture',
                    f'test --locked {selection} --test local_writer_admission -- --nocapture',
                    f'test --locked {selection} --bin okilum local_writer_admission -- --nocapture',
                    f'test --locked {selection} --lib local_writer_admission -- --nocapture',
                    f'test --locked {selection} --lib local_writer_review -- --nocapture',
                    f'test --locked {selection} --bin okilum local_writer_review -- --nocapture',
                ]
                self.assertEqual(calls.read_text().splitlines(), expected if success else expected[:1])
                if passed == '0' and status == 0:
                    self.assertIn('No non-zero passing test count', result.stderr)

    def test_required_target_cannot_borrow_another_target_or_child_footer(self):
        authority = 'Running unittests src/lib.rs (target/debug/deps/okilum_write_authority-abc)\n'
        core = 'Running unittests src/lib.rs (target/debug/deps/okilum_core-def)\n'
        passed = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'
        empty = 'test result: ok. 0 passed; 0 failed; 1 ignored;\n'
        failed = 'test result: FAILED. 1 passed; 1 failed; 0 ignored;\n'
        for receipt, accepted in [
                (authority + passed + core + empty, True),
                (core + passed, False),
                (authority + passed + empty + core + passed, False),
                (authority + passed + failed + core + passed, False),
                (authority + core + passed, False)]:
            with self.subTest(receipt=receipt):
                self.assertEqual(target_tests.passed(receipt, 'okilum_write_authority'), accepted)


if __name__ == "__main__":
    unittest.main()
