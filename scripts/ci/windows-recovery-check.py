#!/usr/bin/env python3
"""Require the actual Windows save/recovery regressions before an agent merge."""
import subprocess


REGRESSION = "file_editor::windows_editor_tests::windows_editor_unacknowledged_commit_retains_history_and_draft"


def main():
    command = ["cargo", "test", "--locked", "-p", "okilum-core", "--lib", "windows_"]
    listing = subprocess.run(command + ["--", "--list"], text=True,
                             stdout=subprocess.PIPE, check=True).stdout
    if f"{REGRESSION}: test" not in listing.splitlines():
        raise RuntimeError("Windows acknowledgement regression did not compile/discover")
    return subprocess.run(command + ["--", "--nocapture", "--test-threads=1"],
                          check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
