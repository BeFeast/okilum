#!/usr/bin/env python3
"""Real native selection/batch inventory with a compiled click-dispatch control."""
from pathlib import Path
import subprocess

QA = Path("qa/tree-batch-2681")
BASE = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin", "okilum"]
REGRESSION = "reader_batch::tests::issue2681_mounted_modified_clicks_select_without_opening_and_show_batch_actions"


def run(label, *args):
    result = subprocess.run([*BASE, *args], text=True, encoding="utf-8", errors="replace",
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    (QA / f"{label}.log").write_text(result.stdout, encoding="utf-8")
    print(result.stdout, flush=True)
    return result


def main():
    QA.mkdir(parents=True, exist_ok=True)
    listed = run("inventory", "issue2681_", "--", "--list")
    assert listed.returncode == 0
    cases = [line.removesuffix(": test") for line in listed.stdout.splitlines() if line.endswith(": test")]
    assert len(cases) >= 4 and REGRESSION in cases, "require the complete new inventory"
    complete = run("complete", "issue2681_", "--", "--nocapture", "--test-threads=1")
    assert complete.returncode == 0 and "0 failed" in complete.stdout
    for case in cases:
        assert f"test {case} ... ok" in complete.stdout, f"case did not execute: {case}"
    source = Path("crates/okilum-shell/src/main.rs")
    original = source.read_bytes()
    condition = b"if modifiers.control || modifiers.platform || modifiers.shift {"
    assert original.count(condition) == 1
    try:
        source.write_bytes(original.replace(condition, b"if false && (modifiers.control || modifiers.platform || modifiers.shift) {", 1))
        negative = run("single-selection-negative", REGRESSION, "--", "--exact", "--nocapture", "--test-threads=1")
        assert negative.returncode != 0 and "running 1 test" in negative.stdout and "0 passed; 1 failed" in negative.stdout
    finally:
        source.write_bytes(original)
    restored = run("restored", REGRESSION, "--", "--exact", "--nocapture", "--test-threads=1")
    assert restored.returncode == 0 and "1 passed; 0 failed" in restored.stdout
    retained = run("retained-single-item", "reader_move::tests::native_preview_cancel_move_and_recovery_guard", "--", "--exact", "--nocapture", "--test-threads=1")
    assert retained.returncode == 0 and "1 passed; 0 failed" in retained.stdout
    assert source.read_bytes() == original


if __name__ == "__main__":
    main()
