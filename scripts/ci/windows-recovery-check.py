#!/usr/bin/env python3
"""Require the actual Windows save/recovery regressions before an agent merge."""
import subprocess


REGRESSION = "file_editor::windows_editor_tests::windows_editor_unacknowledged_commit_retains_history_and_draft"
PARENT_REGRESSION = "windows_editor_changed_parent_keeps_the_latest_queued_draft"


def main():
    command = ["cargo", "test", "--locked", "-p", "okilum-core", "--tests", "windows_"]
    listing = subprocess.run(command + ["--", "--list"], text=True,
                             stdout=subprocess.PIPE, check=True).stdout
    for regression in (REGRESSION, PARENT_REGRESSION):
        if f"{regression}: test" not in listing.splitlines():
            raise RuntimeError(f"Windows regression did not compile/discover: {regression}")
    return subprocess.run(command + ["--", "--nocapture", "--test-threads=1"],
                          check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
