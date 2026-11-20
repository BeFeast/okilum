"""Merge-queue decisions. No git, Forgejo, or build-slot."""
import json
import unittest
from pathlib import Path
import importlib.util

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("merge_queue", ROOT / "scripts/ci/merge-queue.py")
queue = importlib.util.module_from_spec(spec)
spec.loader.exec_module(queue)

HEAD = "a" * 40
OTHER = "b" * 40
MAIN = "c" * 40


def comment(body, user="oklabs-bot", when="2026-11-20T12:00:00Z"):
    if body.split("\n", 1)[0].endswith(HEAD) or "REVIEW PASS" in body.split("\n", 1)[0]:
        if "author family:" not in body and "REVIEW PASS" in body.split("\n", 1)[0]:
            body += "\nReviewer: Codex (gpt-6.1-sol); author family: xai"
        body += "\nReviewer: Codex (gpt-6.1-sol); author family: xai"
    return {"body": body, "user": {"login": user}, "created_at": when}


def pr(number, head=HEAD, labels=(), draft=False, base="main"):
    return {"number": number, "draft": draft, "head": head, "labels": list(labels), "base": base}


def detail(sha=HEAD, branch="agent-checks/topic", linux="success", created="2026-11-20T12:00:00Z"):
    """A run view gh_check.verdict accepts when linux is success."""
    linux_steps = [
        "Prepare and verify pinned vendor and comrak", "Lint every workspace target offline",
        "Build Linux desktop offline", "Run complete okilum-core suite offline",
        "Run Linux Rust suites offline", "Verify agent helper controls",
        "Stress private reveal bus (20 runs)", "Run CI, releases and updater Python suites",
        "Verify per-user AppImage runtime",
        "Verify pinned Sync fixture lifecycle, source sharing and remote inspection",
        "Verify managed launch and configuration admission",
        "Verify real disk-full Sync transfer and first merge",
        "Reject and reap surviving agent fixture daemons",
        "Verify additive vault regression controls",
    ]
    windows_steps = [
        "Prepare and verify Windows vendor", "Run native Windows recovery controls",
        "Run native Windows delta console controls", "Run native Windows Sync identity controls",
        "Run native Windows atlas upload controls", "Run native Windows terminal console controls",
        "Run native Windows Kubernetes transport controls",
        "Run native Windows okilum-cored CLI and MCP suites",
    ]
    return {
        "headSha": sha, "headBranch": branch, "status": "completed",
        "conclusion": "failure" if linux == "failure" else "success",
        "createdAt": created,
        "jobs": [
            {"name": "linux", "conclusion": linux, "steps": [] if linux == "skipped" else [
                {"name": name, "conclusion": "success"} for name in linux_steps]},
            {"name": "windows-recovery", "conclusion": "success", "steps": [
                {"name": name, "conclusion": "success"} for name in windows_steps]},
            {"name": "pre-merge", "conclusion": "skipped", "steps": []},
        ],
    }


def green(branch="agent-checks/topic", sha=HEAD):
    return [detail(sha, branch)]


class ReviewAndGate(unittest.TestCase):
    def test_latest_trusted_pass_on_this_head_is_required(self):
        comments = [
            comment(f"REVIEW PASS {HEAD}", when="2026-11-20T11:00:00Z"),
            comment(f"REVIEW FIX {HEAD}", when="2026-11-20T12:00:00Z"),
        ]
        self.assertEqual(queue.review_verdict(comments, HEAD)[0], "FIX")
        comments.append(comment(f"REVIEW PASS {HEAD[:12]}", when="2026-11-20T13:00:00Z"))
        self.assertEqual(queue.review_verdict(comments, HEAD)[0], "PASS")
        self.assertEqual(queue.review_verdict([comment(f"REVIEW PASS {OTHER}")], HEAD)[0], None)
        self.assertEqual(queue.review_verdict([comment(f"REVIEW PASS {HEAD}", user="stranger")], HEAD)[0], None)
        self.assertEqual(queue.review_verdict([comment(f"CRITICAL-REVIEW PASS {HEAD}")], HEAD)[0], "PASS")
        same = comment(f"REVIEW PASS {HEAD}")
        same["body"] = f"REVIEW PASS {HEAD}\nReviewer: Grok (xai); author family: xai"
        self.assertIsNone(queue.review_verdict([same], HEAD)[0])
        bare = {"body": f"REVIEW PASS {HEAD}", "user": {"login": "oklabs-bot"}, "created_at": "2026-11-20T12:00:00Z"}
        self.assertIsNone(queue.review_verdict([bare], HEAD)[0])

    def test_fast_and_failed_runs_are_not_a_full_gate(self):
        self.assertTrue(queue.full_green(green(), HEAD))
        self.assertFalse(queue.full_green(green("agent-checks-fast/topic"), HEAD))
        self.assertFalse(queue.full_green(green("agent-checks-benchmark/x/topic"), HEAD))
        runs = green() + [detail(HEAD, created="2026-11-20T13:00:00Z", linux="failure")]
        self.assertFalse(queue.full_green(runs, HEAD))
        self.assertFalse(queue.full_green([], HEAD))
        # A green workflow whose linux job was skipped is not a full gate.
        self.assertFalse(queue.full_green([detail(linux="skipped")], HEAD))

    def test_only_a_pull_into_main_can_enter(self):
        comments = {7: [comment(f"REVIEW PASS {HEAD}")]}
        runs = {HEAD: green()}
        self.assertEqual(queue.eligible([pr(7, base="release")], comments, runs), [])
        self.assertEqual(queue.eligible([pr(7)], comments, runs)[0]["number"], 7)
        held = pr(8, labels=["manager-merge"])
        self.assertEqual(queue.eligible([held], {8: [comment(f"REVIEW PASS {HEAD}")]}, runs), [])

    def test_a_fix_that_arrives_during_the_suite_blocks_the_push(self):
        batch = [pr(7)]
        comments = {7: [comment(f"REVIEW PASS {HEAD}")]}
        runs = {HEAD: green()}
        self.assertTrue(queue.still_landable(batch, [pr(7)], comments, runs))
        comments = {7: [comment(f"REVIEW PASS {HEAD}", when="2026-11-20T11:00:00Z"),
                        comment(f"REVIEW FIX {HEAD}", when="2026-11-20T13:00:00Z")]}
        self.assertFalse(queue.still_landable(batch, [pr(7)], comments, runs))
        self.assertFalse(queue.still_landable(batch, [pr(7, head=OTHER)], {7: [comment(f"REVIEW PASS {OTHER}")]}, {OTHER: green(sha=OTHER)}))

    def test_a_failed_read_of_the_newest_run_does_not_reuse_an_older_success(self):
        older = detail(created="2026-11-20T12:00:00Z")
        newer = {"databaseId": 2, "headBranch": "agent-checks/topic", "headSha": HEAD,
                 "status": "completed", "conclusion": "failure",
                 "createdAt": "2026-11-20T13:00:00Z"}
        listed = [older, newer]
        self.assertEqual(queue.newest_full_run(listed)["databaseId"], 2)
        record = queue.runs_after_view(listed, lambda run: None if run["databaseId"] == 2 else run)
        self.assertTrue(record["unreadable"])
        self.assertFalse(queue.gate_ok(record, HEAD))
        self.assertTrue(queue.gate_ok({"unreadable": False, "runs": [older]}, HEAD))

    def test_a_later_comment_page_can_replace_pass_with_fix(self):
        pages = [
            [comment(f"REVIEW PASS {HEAD}", when="2026-11-20T10:00:00Z")],
            [comment(f"REVIEW FIX {HEAD}", when="2026-11-20T12:00:00Z")],
        ]
        loaded = queue.pages(lambda page: pages[page - 1] if page <= len(pages) else [], limit=1)
        self.assertEqual(queue.review_verdict(loaded, HEAD)[0], "FIX")

    def test_eligible_order_is_critical_then_oldest_pass(self):
        prs = [pr(2, HEAD), pr(3, OTHER, labels=["critical"]), pr(4, "d" * 40, draft=True)]
        comments = {
            2: [comment(f"REVIEW PASS {HEAD}", when="2026-11-20T10:00:00Z")],
            3: [comment(f"REVIEW PASS {OTHER}", when="2026-11-20T11:00:00Z")],
            4: [comment("REVIEW PASS " + "d" * 40)],
        }
        runs = {HEAD: green(), OTHER: green(sha=OTHER), "d" * 40: green(sha="d" * 40)}
        chosen = queue.eligible(prs, comments, runs)
        self.assertEqual([item["number"] for item in chosen], [3, 2])


class Landing(unittest.TestCase):
    def test_rebased_head_lands_without_a_second_suite(self):
        self.assertEqual(queue.classify_landing(lambda base, tip: True, MAIN, [HEAD]), "ff")
        self.assertEqual(queue.classify_landing(lambda base, tip: False, MAIN, [HEAD]), "suite")
        self.assertEqual(queue.classify_landing(lambda base, tip: True, MAIN, [HEAD, OTHER]), "suite")

    def test_red_ejects_the_last_pr_and_infra_retries(self):
        batch = [pr(1), pr(2, OTHER)]
        failed = json.dumps([{"name": "Run Linux Rust suites offline", "code": 1}])
        self.assertEqual(queue.suite_outcome(0, batch), ("ff", None))
        self.assertEqual(queue.suite_outcome(75, batch)[0], "retry")
        self.assertEqual(queue.suite_outcome(124, batch, "")[0], "retry")
        self.assertEqual(queue.suite_outcome(137, [pr(1)], "")[0], "retry")
        self.assertEqual(queue.suite_outcome(1, [pr(1)], "")[0], "retry")
        self.assertEqual(queue.suite_outcome(1, batch, failed)[0], "bisect")
        outcome, ejected = queue.suite_outcome(1, [pr(9)], failed)
        self.assertEqual((outcome, ejected["number"]), ("eject", 9))
        phase_deadline = json.dumps([{"name": "Run Linux Rust suites offline", "code": 124}])
        self.assertEqual(queue.suite_outcome(124, [pr(9)], phase_deadline)[0], "eject")
        results = [(pr(1), 1, failed), (pr(2, OTHER), 0, "[]")]
        self.assertEqual([item["number"] for item in queue.bisect_ejections(results)], [1])
        self.assertEqual(queue.skip_contained([pr(1), pr(2, OTHER)], {HEAD}), [pr(2, OTHER)])
        self.assertEqual(queue.note_infra({"infra": {}}, [pr(1)], MAIN), 1)
        self.assertTrue(queue.oom_killed('{"oom_kill": 1}'))
        self.assertFalse(queue.oom_killed('{"oom_kill": 0}'))

    def test_ejection_names_the_failing_phase_and_does_not_retry_the_same_base(self):
        receipt = json.dumps([
            {"name": "Lint every workspace target offline", "code": 0},
            {"name": "Run Linux Rust suites offline", "code": 1},
        ])
        self.assertEqual(queue.failing_phase(receipt)[0], "Run Linux Rust suites offline")
        text = queue.eject_comment(pr(9), MAIN, "e" * 40, queue.failing_phase(receipt), "assertion failed")
        self.assertIn("ejected PR #9", text)
        self.assertIn("Run Linux Rust suites offline", text)
        self.assertIn("assertion failed", text)
        state = {"ejected": [queue.ejection_key(9, HEAD, MAIN)]}
        self.assertTrue(queue.is_ejected(state, 9, HEAD, MAIN))
        self.assertFalse(queue.is_ejected(state, 9, HEAD, OTHER))
        self.assertFalse(queue.is_ejected(state, 9, "f" * 40, MAIN))


class PoolScripts(unittest.TestCase):
    def test_pre_merge_script_does_not_run_rust_suites(self):
        script = (ROOT / "scripts/ci/pool-pre-merge.sh").read_text()
        self.assertIn("cargo clippy --workspace --all-targets --locked -- -D warnings", script)
        check = next(line.strip() for line in script.splitlines() if line.strip().startswith("cargo check --release"))
        self.assertIn("--bin okilum-sync-launch", check)
        for heavy in ("cargo build", "cargo test", "cargo run", "nextest",
                      "at-vault-regression-control", "linux-rust-tests"):
            self.assertNotIn(heavy, script)

    def test_linux_gate_keeps_the_merge_rule_phase_names(self):
        script = (ROOT / "scripts/ci/pool-linux-gate.sh").read_text()
        for name in ("Run Linux Rust suites offline", "Run complete okilum-core suite offline",
                     "Lint every workspace target offline", "Build Linux desktop offline"):
            self.assertIn(name, script)
        self.assertIn("bash scripts/ci/linux-rust-tests.sh", script)
        dispatch = (ROOT / "scripts/ci/pool-dispatch.sh").read_text()
        self.assertIn("/home/god/scratch/okilum-pool-receipts", (ROOT / "scripts/ci/pool-phase-lib.sh").read_text())
        self.assertIn("pool_receipt_dir", dispatch)
        self.assertIn("pool_ensure_git", (ROOT / "scripts/ci/pool-linux-gate.sh").read_text())
        self.assertIn("pool_ensure_git", (ROOT / "scripts/ci/pool-pre-merge.sh").read_text())
        unit = (ROOT / "scripts/ci/systemd/okilum-merge-queue.service").read_text()
        start = next(line for line in unit.splitlines() if line.startswith("ExecStart="))
        command = start.split("=", 1)[1]
        self.assertIn('"/bin/bash -lc \'cd /home/god/src/okilum-merge-queue && ', command)
        self.assertTrue(command.endswith("'\""))


if __name__ == "__main__":
    unittest.main()
