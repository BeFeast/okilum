"""Controls for source-pinned hosted worker verdicts (no compiler or network)."""
import copy
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("gh_check", ROOT / "scripts/agent/gh_check.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
SHA = "a" * 40


def receipt():
    return {"headSha": SHA, "status": "completed", "conclusion": "success", "jobs": [
        {"name": "linux", "conclusion": "success", "steps": [
            {"name": n, "conclusion": "success"} for n in (
                "Prepare and verify pinned vendor and comrak",
                "Run complete okilum-core suite offline", "Run full Linux gate offline")]}]}


class VerdictTests(unittest.TestCase):
    def test_positive_control_all_required_checks_pass(self):
        self.assertEqual(helper.verdict(receipt(), SHA), 0)

    def test_other_head_or_missing_tests_cannot_pass(self):
        self.assertEqual(helper.verdict(receipt(), "b" * 40), 75)
        for index in range(3):
            data = receipt()
            data["jobs"][0]["steps"].pop(index)
            self.assertEqual(helper.verdict(data, SHA), 75)

    def test_failure_vs_infrastructure_and_cancelled(self):
        for name, expected in (("Run full Linux gate offline", 1),
                               ("Run complete okilum-core suite offline", 1),
                               ("Prepare and verify pinned vendor and comrak", 1),
                               ("Install system libraries", 75),
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

    def test_partial_empty_or_skipped_jobs_are_not_success(self):
        for jobs in ([], [{"name": "linux", "conclusion": "skipped", "steps": []}]):
            data = receipt()
            data["jobs"] = copy.deepcopy(jobs)
            self.assertEqual(helper.verdict(data, SHA), 75)

    def test_worker_lane_has_no_secrets_or_publication(self):
        workflow = (ROOT / ".github/workflows/agent-checks.yml").read_text()
        self.assertNotIn("secrets.", workflow)
        self.assertIn("branches: ['agent-checks/**']", workflow)
        self.assertIn("persist-credentials: false", workflow)
        self.assertIn("contents: read", workflow)
        self.assertIn("cancel-in-progress: true", workflow)
        self.assertIn("runs-on: ubuntu-24.04", workflow)
        for other in (ROOT / ".github/workflows").glob("*.yml"):
            if other.name != "agent-checks.yml":
                self.assertNotIn("agent-checks/", other.read_text(), other.name)


if __name__ == "__main__":
    unittest.main()
