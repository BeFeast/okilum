"""Verify-only cannot reach publication; source/dispatch admission fails closed."""
from datetime import datetime
import importlib.util
import os
from pathlib import Path
import re
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

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
    def test_hosted_checkout_uses_only_authenticated_exact_mirror_ref(self):
        import json
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            ref = 'okilum-verify-source/2218-' + SOURCE + '-700'
            environment = env() | dict(HOSTED_VERIFY='true', SHIPPING_BUILD='5700',
                VERIFY_EVIDENCE=json.dumps({'ref': ref}), VERIFY_GITHUB_TOKEN='fixture-token',
                GITHUB_WORKSPACE=tmp, RUNNER_TEMP=tmp, ACTIONS_RUNTIME_TOKEN='must-not-pass')
            commands = []
            def command(*args, **kwargs):
                commands.append((args, kwargs))
                sha = SOURCE if kwargs.get('cwd') == root / 'verify-product' else WORKFLOW
                return SimpleNamespace(stdout=sha + '\n')
            def build(*args, **kwargs):
                (root / 'native-verify').mkdir()
            with patch.object(verify, 'admission', return_value=SOURCE), \
                 patch.object(verify.sys, 'platform', 'darwin'), \
                 patch.object(verify, 'command', side_effect=command), \
                 patch.object(verify.subprocess, 'run', side_effect=build) as product, \
                 patch.object(verify, 'verify_artifact', return_value={'verify_only': True}):
                verify.execute(environment)
            fetches = [(a, kw) for a, kw in commands if 'fetch' in a]
            self.assertEqual(len(fetches), 1)
            self.assertEqual(fetches[0][0][-1], 'refs/heads/' + ref)
            self.assertIn('GIT_CONFIG_VALUE_0', fetches[0][1]['env'])
            self.assertFalse(any('git.oklabs.uk' in str(a) for a, _ in commands))
            build_env = product.call_args.kwargs['env']
            self.assertEqual(build_env['OKILUM_BUILD_VERSION'], '5700')
            self.assertNotIn('VERIFY_GITHUB_TOKEN', build_env)
            self.assertNotIn('ACTIONS_RUNTIME_TOKEN', build_env)

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

    def test_verify_only_workflow_uses_hosted_bridge_without_feed_credentials(self):
        import yaml
        repo = Path(__file__).resolve().parents[2]
        text = (repo / '.forgejo/workflows/macos-native-verify.yml').read_text()
        workflow = yaml.safe_load(text)
        self.assertEqual(list(workflow['jobs']), ['verify-only'])
        job = workflow['jobs']['verify-only']
        self.assertEqual(job['runs-on'], 'bridge')
        self.assertIn("inputs.verify_only == 'true'", job['if'])
        self.assertEqual(job['steps'][-1]['run'], 'python3 scripts/ci/github-macos-shipping.py')
        for forbidden in ['SPARKLE_ED_PRIVATE_KEY', 'R2_', 'publication.py', 'publish.py']:
            self.assertNotIn(forbidden, text)
        hosted = (repo / '.github/workflows/forgejo-macos-shipping.yml').read_text()
        self.assertIn('python3 scripts/ci/macos-native-verify.py', hosted)
        self.assertIn('macos-verify', hosted)
        self.assertNotIn('publication.py', hosted)

    def test_full_verify_only_execution_builds_once_without_publisher_or_write_api(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            environment = {**env(), "GITHUB_WORKSPACE": str(root), "RUNNER_TEMP": str(root),
                           "R2_SECRET_ACCESS_KEY": "forbidden", "GITHUB_TOKEN": "forbidden",
                           "SPARKLE_ED_PRIVATE_KEY": "forbidden",
                           "FUTURE_CREDENTIAL": "unknown-secret", "ACTIONS_RUNTIME_TOKEN": "upload-secret",
                           "OKILUM_SIGNING_IDENTITY": "approved identity", "HOME": str(root)}
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
            for key in ("R2_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "SPARKLE_ED_PRIVATE_KEY",
                        "FUTURE_CREDENTIAL", "ACTIONS_RUNTIME_TOKEN"):
                self.assertNotIn(key, child_env)
            self.assertEqual(child_env["OKILUM_SIGNING_IDENTITY"], "approved identity")
            self.assertEqual(child_env["HOME"], str(root))
            self.assertTrue(set(child_env) <= verify.BUILD_ENVIRONMENT | {"OKILUM_BUILD_VERSION"})
            self.assertTrue((root / "native-verify/native-verify-receipt.json").is_file())


if __name__ == "__main__":
    unittest.main()
