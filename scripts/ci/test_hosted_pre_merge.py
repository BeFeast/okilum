"""Hosted fast execution/reporting controls; no Cargo or build-slot runs."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("hosted_pre_merge", HERE / "hosted-pre-merge.py")
hosted = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hosted)


@unittest.skipUnless(sys.platform == "linux", "Linux hosted execution")
class HostedPreMerge(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        script = self.root / "scripts/ci/pool-pre-merge.sh"
        script.parent.mkdir(parents=True)
        script.write_text('''#!/usr/bin/env bash
python3 - <<'PY'
import os,pathlib
if os.environ['PHASES'] != 'MISSING':
    pathlib.Path(os.environ['OKILUM_POOL_RECEIPT']).write_text(os.environ['PHASES'])
PY
exit "$PHASE_EXIT"
''')
        self.env = dict(GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted",
                        OKILUM_SOURCE_COMMIT="a" * 40, RUNNER_TEMP=str(self.root),
                        GITHUB_ENV=str(self.root / "environment"),
                        GITHUB_OUTPUT=str(self.root / "outputs"))

    def execute(self, phases, status=0):
        data = phases if isinstance(phases, str) else json.dumps(phases)
        with patch.dict(os.environ, dict(self.env, PHASES=data, PHASE_EXIT=str(status))), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return hosted.main(self.root)

    def phase_result(self, name):
        outputs = dict(line.split("=", 1) for line in (self.root / "environment").read_text().splitlines())
        return subprocess.run(["bash", str(HERE / "pool-phase.sh"), name],
                              env=dict(os.environ, **outputs), capture_output=True, text=True, timeout=5)

    def test_outside_hosted_runner_is_rejected_before_execution(self):
        for env in ({"GITHUB_ACTIONS": "false"}, {"RUNNER_ENVIRONMENT": "self-hosted"},
                    {"OKILUM_SOURCE_COMMIT": "not-a-sha"}):
            with self.subTest(env=env), patch.dict(os.environ, dict(self.env, **env)), \
                    patch.object(hosted.subprocess, "run") as execute, \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(hosted.main(self.root), 75)
                execute.assert_not_called()

    def test_pass_receipt_retains_source_and_named_phase_reporting(self):
        phases = [{"name": "Check formatting", "code": 0},
                  {"name": "Pool shard complete", "code": 0}]
        self.assertEqual(self.execute(phases), 0)
        self.assertEqual((self.root / "outputs").read_text(), "kind=verdict\n")
        self.assertEqual(self.phase_result("Check formatting").returncode, 0)
        self.assertEqual(self.phase_result("--overall").returncode, 0)
        env = dict(line.split("=", 1) for line in (self.root / "environment").read_text().splitlines())
        execution = json.loads(Path(env["OKILUM_POOL_SHARDS"]).read_text())
        self.assertEqual(execution[0]["shard"], "github-hosted")
        self.assertEqual(execution[0]["source"], "a" * 40)

    def test_failed_or_missing_phase_cannot_be_reported_as_pass(self):
        for phases, status in (([{"name": "Check formatting", "code": 1},
                                 {"name": "Pool shard complete", "code": 1}], 1),
                               ([{"name": "Check formatting", "code": None, "skipped": True},
                                 {"name": "Pool shard complete", "code": 1}], 1),
                               ([{"name": "Pool shard complete", "code": 0}], 0)):
            with self.subTest(phases=phases):
                self.assertEqual(self.execute(phases, status), 0)
                self.assertNotEqual(self.phase_result("Check formatting").returncode, 0)
                if status:
                    self.assertNotEqual(self.phase_result("--overall").returncode, 0)

    def test_missing_malformed_truncated_or_inconsistent_receipts_are_infrastructure(self):
        for phases, status in (("MISSING", 0), ("{bad", 0),
                               ([{"name": "Check formatting", "code": 0}], 0),
                               ([{"name": "Pool shard complete", "code": 1}], 0)):
            with self.subTest(phases=phases):
                self.assertEqual(self.execute(phases, status), 75)
                self.assertFalse((self.root / "outputs").exists())

    def test_timeout_cannot_pass_even_with_a_preexisting_complete_receipt(self):
        def timed_out(command, **options):
            Path(options["env"]["OKILUM_POOL_RECEIPT"]).write_text(
                '[{"name":"Pool shard complete","code":0}]')
            self.assertEqual(command[2:4], ["hosted-pre-merge", "1800"])
            return subprocess.CompletedProcess(command, 124)
        with patch.dict(os.environ, self.env), patch.object(hosted.subprocess, "run", side_effect=timed_out), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(hosted.main(self.root), 75)
        self.assertFalse((self.root / "outputs").exists())
