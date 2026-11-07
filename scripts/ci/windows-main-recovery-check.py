#!/usr/bin/env python3
"""Require the three original Windows main failures and restored runtime passes."""
from pathlib import Path
import subprocess
import sys

BASELINE = "f37ca5a8ca7567bd9cf0cfae1c71c56a84f7b90d"
QA = Path("qa/windows-main-recovery-2700")
CORE = Path("crates/okilum-core/tests/desktop_editor.rs")
SHELL = Path("crates/okilum-shell/src/reader_deleted_note.rs")
CASES = (
    (CORE, "okilum-core", ["--test", "desktop_editor"],
     "windows_editor_changed_parent_keeps_the_latest_queued_draft", "Vault root replaced"),
    (SHELL, "okilum-shell", ["--bins"],
     "reader_deleted_note::tests::dont_save_cannot_leave_when_latest_recovery_write_fails",
     "Access is denied"),
    (SHELL, "okilum-shell", ["--bins"],
     "reader_deleted_note::tests::window_mode_switch_keeps_deleted_editor_on_cancel_and_failed_recovery",
     "Access is denied"),
)


def run(case, label, negative=False):
    _, package, target, test, original_error = case
    result = subprocess.run(
        ["cargo", "test", "--locked", "-p", package, *target, test,
         "--", "--exact", "--nocapture", "--test-threads=1"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        encoding="utf-8", errors="replace", check=False,
    )
    (QA / f"{test.rsplit('::', 1)[-1]}-{label}.log").write_text(result.stdout, encoding="utf-8")
    print(result.stdout, flush=True)
    assert "running 1 test" in result.stdout, "control did not execute"
    if negative:
        assert result.returncode != 0 and "1 failed" in result.stdout
        assert original_error in result.stdout, "did not reproduce the original failure"
    else:
        assert result.returncode == 0 and "1 passed; 0 failed" in result.stdout


def main():
    assert sys.platform == "win32", "original failure controls require Windows"
    QA.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "fetch", "--no-tags", "origin", BASELINE], check=True)
    originals = {path: path.read_bytes() for path in (CORE, SHELL)}
    try:
        for path in originals:
            path.write_bytes(subprocess.check_output(["git", "show", f"{BASELINE}:{path.as_posix()}"]))
        for case in CASES:
            run(case, "original-negative", negative=True)
    finally:
        for path, content in originals.items():
            path.write_bytes(content)
    for case in CASES:
        run(case, "restored")
    for path, content in originals.items():
        assert path.read_bytes() == content


if __name__ == "__main__":
    main()
