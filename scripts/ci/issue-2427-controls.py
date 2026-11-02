#!/usr/bin/env python3
"""Validation-branch-only removal controls for #2427; never shipped in its PR."""
import subprocess
from pathlib import Path

warm = Path("crates/okilum-core/src/vault/warm.rs")
editor = Path("crates/okilum-core/src/file_editor.rs")
original = {path: path.read_bytes() for path in [warm, editor]}
cases = [
    "vault::warm::tests::aged_native_revision_reuses_fingerprinted_source_but_new_checks_require_proof",
    "file_editor::tests::recovery_discovery_keeps_deleted_source_drafts_and_checks_returned_bytes",
]

def run(case, expected_failure):
    result = subprocess.run(["cargo", "test", "--offline", "--locked", "-p", "okilum-core", "--lib", case, "--", "--exact", "--nocapture"], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, flush=True)
    if expected_failure:
        assert result.returncode == 101 and "test result: FAILED. 0 passed; 1 failed" in result.stdout, "removal control must fail in its assertion, not compilation/setup"
    else:
        assert result.returncode == 0 and "test result: ok. 1 passed" in result.stdout, "fixed control must execute and pass"

try:
    text = warm.read_text()
    assert "if !before.matches_saved(saved) {" in text
    warm.write_text(text.replace("if !before.matches_saved(saved) {", "if before != saved {", 1))
    text = editor.read_text()
    start = text.index("        let path = match path.canonicalize()", text.index("pub fn has_unsaved_draft"))
    end = text.index("        let key =", start)
    editor.write_text(text[:start] + "        let path = path.canonicalize()?;\n" + text[end:])
    for case in cases:
        print("REMOVAL_CONTROL expected assertion failure: " + case, flush=True)
        run(case, True)
finally:
    for path, content in original.items():
        path.write_bytes(content)
for case in cases:
    print("FIXED_CONTROL: " + case, flush=True)
    run(case, False)
print("ISSUE_2427_REMOVAL_CONTROLS_PASS", flush=True)
