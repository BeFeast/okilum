"""Verify-only cannot reach publication; source/dispatch admission fails closed."""
from datetime import datetime
import importlib.util
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import yaml

SPEC = importlib.util.spec_from_file_location("macos_native_verify",
    Path(__file__).with_name("macos-native-verify.py"))
verify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify)
SOURCE = "a" * 40
WORKFLOW = "b" * 40


def env():
    return dict(VERIFY_ONLY="true", VERIFY_SOURCE=SOURCE, VERIFY_PR="2004",
                GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_REPOSITORY=verify.REPO,
                GITHUB_REF="refs/heads/main", GITHUB_SHA=WORKFLOW,
                GITHUB_RUN_ATTEMPT="1", GITHUB_RUN_NUMBER="1")


def api(path):
    if path == "/pulls/2004":
        return dict(state="open", draft=False, merged=False,
                    base=dict(ref="main", repo=dict(full_name=verify.REPO)),
                    head=dict(sha=SOURCE, repo=dict(full_name=verify.REPO)))
    if path == "/commits/" + SOURCE + "/status":
        return dict(sha=SOURCE, statuses=[dict(context=c, status="success") for c in verify.REQUIRED])
    raise AssertionError("unexpected API (including any publication path): " + path)


class NativeVerifyTests(unittest.TestCase):
    def test_only_explicit_first_attempt_exact_green_trusted_pr_enters(self):
        self.assertEqual(verify.admission(env(), api), SOURCE)
        for key, value in (("VERIFY_ONLY", "false"), ("VERIFY_ONLY", ""),
                           ("GITHUB_RUN_ATTEMPT", "2"), ("GITHUB_REF", "refs/heads/pr"),
                           ("GITHUB_EVENT_NAME", "push"), ("VERIFY_SOURCE", "bad"),
                           ("GITHUB_REPOSITORY", "fork/repo")):
            with self.subTest(key=key), self.assertRaises(ValueError):
                verify.admission({**env(), key: value}, api)
        def red(path):
            result = api(path)
            if path.endswith("/status"):
                result["statuses"][0]["status"] = "pending"
            return result
        with self.assertRaises(ValueError):
            verify.admission(env(), red)
        def stale(path):
            result = api(path)
            if path.startswith("/pulls/"):
                result["head"]["sha"] = "c" * 40
            return result
        with self.assertRaises(ValueError):
            verify.admission(env(), stale)

    def test_verify_only_workflow_has_no_reachable_publication_steps_or_secrets(self):
        repo = Path(__file__).resolve().parents[2]
        text = (repo / ".forgejo/workflows/macos-native-verify.yml").read_text()
        workflow = yaml.safe_load(text)
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertEqual(set(workflow["jobs"]), {"verify-only"})
        job = workflow["jobs"]["verify-only"]
        self.assertIn("inputs.verify_only == 'true'", job["if"])
        self.assertEqual(job["runs-on"], "macos")
        self.assertFalse(workflow["concurrency"]["cancel-in-progress"])
        self.assertEqual(len(job["steps"]), 4)
        commands = [s.get("run", "") for s in job["steps"]]
        self.assertEqual(commands[1], "python3 scripts/ci/macos-native-verify.py")
        self.assertTrue(commands[0].lstrip().startswith("set -euo pipefail"))
        self.assertTrue(commands[2].lstrip().startswith("set -euo pipefail"))
        self.assertEqual(job["steps"][3]["uses"], "./.forgejo-local-actions/upload-artifact")
        for forbidden in ("secrets.", "publication.py", "publish.py", "macos-artifact.py",
                          "releases.yml", "macos-release.yml", "appcast", "R2_"):
            self.assertNotIn(forbidden, text)

    def test_full_verify_only_execution_builds_once_without_publisher_or_write_api(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            environment = {**env(), "GITHUB_WORKSPACE": str(root), "RUNNER_TEMP": str(root),
                           "R2_SECRET_ACCESS_KEY": "forbidden", "GITHUB_TOKEN": "forbidden",
                           "SPARKLE_ED_PRIVATE_KEY": "forbidden"}
            calls = []
            def build(args, **kwargs):
                calls.append((args, kwargs))
                (root / "native-verify").mkdir()
                return SimpleNamespace(returncode=0)
            noon = SimpleNamespace(now=lambda zone: datetime(2026, 10, 30, 12, 0))
            with patch.object(verify.sys, "platform", "darwin"), \
                 patch.object(verify, "datetime", noon), \
                 patch.object(verify, "command", return_value=SimpleNamespace(stdout=WORKFLOW + "\n")) as cmd, \
                 patch.object(verify.subprocess, "run", side_effect=build), \
                 patch.object(verify, "verify_artifact", return_value={"verify_only": True}):
                # Source identity checks must distinguish main tools from product.
                def command(*args, **kwargs):
                    value = SOURCE if kwargs.get("cwd") == root / "verify-product" else WORKFLOW
                    return SimpleNamespace(stdout=value + "\n")
                cmd.side_effect = command
                verify.execute(environment, api)
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][0][:2], ["bash", "scripts/build-macos-ci.sh"])
            child_env = calls[0][1]["env"]
            for key in ("R2_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "SPARKLE_ED_PRIVATE_KEY"):
                self.assertNotIn(key, child_env)
            self.assertTrue((root / "native-verify/native-verify-receipt.json").is_file())


if __name__ == "__main__":
    unittest.main()
