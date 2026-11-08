#!/usr/bin/env python3
"""Execute native Feedback inventory and a real missing-private-field control."""
from pathlib import Path
import subprocess
import sys

# Windows hosted Python otherwise encodes assertion diagnostics as cp1252.
sys.stdout.reconfigure(encoding="utf-8", errors="replace")

QA = Path("qa/feedback-identity-2683")
BASE = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin", "okilum"]
REGRESSION = "reader_feedback::model::tests::feedback2683_identity_uses_private_parts_and_is_omitted_when_name_is_empty"
FAST_REPORTING = "reader_feedback::ui_tests::feedback2749_"


def run(label, *args):
    result = subprocess.run([*BASE, *args], text=True, encoding="utf-8", errors="replace",
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    (QA / f"{label}.log").write_text(result.stdout, encoding="utf-8")
    print(result.stdout, flush=True)
    return result


def fast_reporting_controls():
    path = Path("crates/okilum-shell/src/reader_feedback.rs")
    original = path.read_bytes()
    description = original.index(b'            .child(\n                div()\n                    .debug_selector(|| "feedback-description"')
    reporter = original.index(b'            .child(div().debug_selector(|| "feedback-reporter"')
    metadata = original.index(b'            .child(\n                div()\n                    .debug_selector(|| "feedback-metadata"', reporter)
    name_first = original[:description] + original[reporter:metadata] + original[description:reporter] + original[metadata:]
    keyboard = b"if action.secondary || action.shift {"
    composition = b"if this.text.read(cx).ime_settling(cx) {"
    assert original.count(keyboard) == original.count(composition) == 1
    cases = [
        ("name-first", "description_precedes_name_and_keeps_initial_focus", name_first, "description precedes name"),
        ("reversed-keys", "description_keys_send_once_and_name_enter_does_not_send",
         original.replace(keyboard, b"if !action.secondary || action.shift {"), "newline must not send"),
        ("unguarded-ime", "composition_settle_and_busy_guards",
         original.replace(composition, b"if false {"), "composition cannot send"),
    ]
    try:
        for label, test, mutated, assertion in cases:
            path.write_bytes(mutated)
            negative = run("fast-report-" + label, FAST_REPORTING + test,
                           "--", "--exact", "--nocapture", "--test-threads=1")
            assert negative.returncode != 0 and "running 1 test" in negative.stdout
            assert "0 passed; 1 failed" in negative.stdout and assertion in negative.stdout, label
    finally:
        path.write_bytes(original)
    restored = run("fast-report-restored", FAST_REPORTING, "--", "--nocapture", "--test-threads=1")
    assert restored.returncode == 0 and "3 passed; 0 failed" in restored.stdout


def main():
    QA.mkdir(parents=True, exist_ok=True)
    listed = run("inventory", "reader_feedback::", "--", "--list")
    assert listed.returncode == 0 and f"{REGRESSION}: test" in listed.stdout.splitlines()
    assert "reader_feedback::ui_tests::feedback2683_mounted_reporter_persists_privately_and_clears_without_changing_install: test" in listed.stdout.splitlines()
    assert sum(line.startswith(FAST_REPORTING) and line.endswith(": test") for line in listed.stdout.splitlines()) == 3
    complete = run("complete", "reader_feedback::", "--", "--nocapture", "--test-threads=1")
    assert complete.returncode == 0 and "0 failed" in complete.stdout
    path = Path("crates/okilum-shell/src/reader_feedback/model.rs")
    original = path.read_bytes()
    field = b'.text("install_id", report.identity.install_id.to_string())'
    assert original.count(field) == 1
    try:
        path.write_bytes(original.replace(field, b""))
        negative = run("missing-field-negative", REGRESSION, "--", "--exact", "--nocapture", "--test-threads=1")
        assert negative.returncode != 0 and "running 1 test" in negative.stdout and "0 passed; 1 failed" in negative.stdout
        assert "install_id" in negative.stdout, "missing-field assertion did not fire"
    finally:
        path.write_bytes(original)
    restored = run("restored", REGRESSION, "--", "--exact", "--nocapture", "--test-threads=1")
    assert restored.returncode == 0 and "1 passed; 0 failed" in restored.stdout
    fast_reporting_controls()


if __name__ == "__main__":
    main()
