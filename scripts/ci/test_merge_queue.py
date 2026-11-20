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
    return {"body": body, "user": {"login": user}, "created_at": when}


def pr(number, head=HEAD, labels=(), draft=False):
    return {"number": number, "draft": draft, "head": head, "labels": list(labels)}


def green(branch="agent-checks/topic"):
    return [{"headBranch": branch, "status": "completed", "conclusion": "success",
             "createdAt": "2026-11-20T12:00:00Z"}]


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

    def test_fast_and_failed_runs_are_not_a_full_gate(self):
        self.assertTrue(queue.full_green(green()))
        self.assertFalse(queue.full_green(green("agent-checks-fast/topic")))
        self.assertFalse(queue.full_green(green("agent-checks-benchmark/x/topic")))
        runs = green() + [{"headBranch": "agent-checks/topic", "status": "completed",
                           "conclusion": "failure", "createdAt": "2026-11-20T13:00:00Z"}]
        self.assertFalse(queue.full_green(runs))
        self.assertFalse(queue.full_green([]))

    def test_eligible_order_is_critical_then_oldest_pass(self):
        prs = [pr(2, HEAD), pr(3, OTHER, labels=["critical"]), pr(4, "d" * 40, draft=True)]
        comments = {
            2: [comment(f"REVIEW PASS {HEAD}", when="2026-11-20T10:00:00Z")],
            3: [comment(f"REVIEW PASS {OTHER}", when="2026-11-20T11:00:00Z")],
            4: [comment("REVIEW PASS " + "d" * 40)],
        }
        runs = {HEAD: green(), OTHER: green(), "d" * 40: green()}
        chosen = queue.eligible(prs, comments, runs)
        self.assertEqual([item["number"] for item in chosen], [3, 2])


class Landing(unittest.TestCase):
    def test_rebased_head_lands_without_a_second_suite(self):
        self.assertEqual(queue.classify_landing(lambda base, tip: True, MAIN, [HEAD]), "ff")
        self.assertEqual(queue.classify_landing(lambda base, tip: False, MAIN, [HEAD]), "suite")
        self.assertEqual(queue.classify_landing(lambda base, tip: True, MAIN, [HEAD, OTHER]), "suite")

    def test_red_ejects_the_last_pr_and_infra_retries(self):
        batch = [pr(1), pr(2, OTHER)]
        self.assertEqual(queue.suite_outcome(0, batch), ("ff", None))
        self.assertEqual(queue.suite_outcome(75, batch)[0], "retry")
        self.assertEqual(queue.suite_outcome(124, batch)[0], "retry")
        outcome, ejected = queue.suite_outcome(1, batch)
        self.assertEqual((outcome, ejected["number"]), ("eject", 2))

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


if __name__ == "__main__":
    unittest.main()
