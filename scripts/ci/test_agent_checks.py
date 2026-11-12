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
                "Reject and reap surviving agent fixture daemons", "Verify additive vault regression controls")]},
        {"name": "windows-recovery", "conclusion": "success", "steps": [
            {"name": n, "conclusion": "success"} for n in (
                "Prepare and verify Windows vendor", "Run native Windows recovery controls",
                "Run native Windows delta console controls", "Run native Windows Sync identity controls",
                "Run native Windows atlas upload controls")]}]}


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
        names = [f'case::{i}' for i in range(80)] + ['case::0', 'case::0::nested']
        inventory = subprocess.CompletedProcess([], 0, stdout=''.join(
            name + ': test\n' for name in names), stderr='')
        for failed in (None, 'ordinary-2'):
            seen = []
            barrier = threading.Barrier(4)
            lock = threading.Lock()

            def execute(name, arguments, expected, *, command):
                self.assertEqual(command, ['cargo', 'test', '--workspace', '--locked',
                                           '--no-fail-fast', '--'])
                self.assertIn('--test-threads=2', arguments)
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

    def test_canonical_compile_jobs_do_not_match_light_or_interactive_runners(self):
        # The shared ubuntu-latest label spans incompatible resource classes.
        # Evaluate scheduler eligibility, including overlapping Docker labels.
        pools = {
            'heavy': {'heavy', 'docker', 'ubuntu-latest'},
            'light': {'light', 'ubuntu-latest'},
            'interactive': {'ci-interactive', 'docker', 'ubuntu-latest'},
        }
        workflows = [('.forgejo/workflows/ci.yml', ('shared-local', 'linux-local')),
                     ('.forgejo/workflows/cold-writer-diagnostic.yml', ('cold-writer',))]
        for path, jobs in workflows:
            workflow = yaml.safe_load((ROOT / path).read_text())
            for name in jobs:
                with self.subTest(job=name):
                    labels = workflow['jobs'][name]['runs-on']
                    required = set(labels if isinstance(labels, list) else [labels])
                    eligible = [pool for pool, available in pools.items()
                                if required <= available]
                    self.assertEqual(eligible, ['heavy'])

    def test_positive_control_all_required_checks_pass(self):
        self.assertEqual(helper.verdict(receipt(), SHA), 0)
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

    def test_vault_regression_control_must_run_for_normal_worker_checks(self):
        info = receipt()
        for verdict in ("skipped", "failure"):
            info["jobs"][0]["steps"][-1]["conclusion"] = verdict
            self.assertEqual(helper.verdict(info, SHA), 75)

    def test_worker_lane_has_no_secrets_or_publication(self):
        workflow = (ROOT / ".github/workflows/agent-checks.yml").read_text()
        self.assertNotIn("secrets.", workflow)
        self.assertIn("branches: ['agent-checks/**', 'agent-checks-benchmark/**']", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("contents: read", workflow)
        self.assertIn("cancel-in-progress: true", workflow)
        self.assertIn("runs-on: ubuntu-24.04", workflow)
        for other in (ROOT / ".github/workflows").glob("*.yml"):
            if other.name != "agent-checks.yml":
                self.assertNotIn("agent-checks/", other.read_text(), other.name)


class HelperFlowTests(unittest.TestCase):
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
                         ["okilum-core", "okilum-shell", "okilum-sync", "okilum-t3"])
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

    def test_linux_suites_keep_the_first_failure_and_still_execute_later_checks(self):
        with tempfile.TemporaryDirectory() as directory:
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
                             'if [[ " $* " == *" -p okilum-core -p okilum-shell -p okilum-sync -p okilum-t3 "* ]]; then\n'
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
            counts = []
            for fail in ('0', '1'):
                log = root / ('calls-' + fail)
                groups = root / ('groups-' + fail)
                result = subprocess.run(['bash', str(scripts / 'linux-rust-tests.sh')], cwd=root,
                                        env={**os.environ, 'PATH': str(fake) + os.pathsep + os.environ['PATH'],
                                             'CALL_LOG': str(log), 'GROUP_LOG': str(groups), 'FAIL_FIRST': fail,
                                             'FAIL_MARKER': str(root / 'failed')},
                                        capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, int(fail), result.stdout + result.stderr)
                calls = log.read_text().splitlines()
                self.assertGreater(len(calls), 20)  # positive control: the full list fired
                self.assertTrue(any('sprint=1' in call for call in calls))
                self.assertTrue(any('retired-sync.test.js' in call for call in calls))
                self.assertIn('vendor-tests', calls)
                self.assertIn('sidecar::macos::preparation::', calls[-1])
                wrapped = groups.read_text().splitlines()
                workspace_calls = [call for call in calls if
                                   '-p okilum-core -p okilum-shell -p okilum-sync -p okilum-t3' in call]
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
                .replace('bash scripts/ci/test-group.sh "$group"',
                         'bash scripts/ci/test-group.sh --kill-after 0.3 "$group"'))
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
