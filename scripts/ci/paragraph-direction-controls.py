#!/usr/bin/env python3
"""Prove the old full-row copy fails the real editor's direction-work bound."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time


TEST = ("reader_editor::code_operations::row_layout_tests::"
        "edit2803_paragraph_direction_does_not_copy_reused_long_note_rows")
COMMAND = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin", "okilum"]
STATE = Path("vendor/gpui-component/crates/base/src/input/base/state.rs")
SUPERVISOR = Path(__file__).with_name("test-group.py").resolve()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def restore(evidence):
    backup = evidence / "candidate-state.rs"
    journal = evidence / "source.json"
    if not backup.exists() and not journal.exists():
        print("PARAGRAPH_DIRECTION_RESTORE_NOT_NEEDED", flush=True)
        return
    data = backup.read_bytes()
    expected = json.loads(journal.read_text())["sha256"]
    assert digest(data) == expected, "candidate backup hash mismatch"
    STATE.write_bytes(data)
    assert digest(STATE.read_bytes()) == expected, "candidate restoration failed"
    print(f"PARAGRAPH_DIRECTION_RESTORED sha256={expected}", flush=True)


def run(evidence, label, limit, command, *, cwd=None):
    started = time.monotonic()
    print(f"PARAGRAPH_DIRECTION_PHASE_START phase={label}", flush=True)
    args = [sys.executable, str(SUPERVISOR), f"paragraph-{label}", str(limit), *command]
    lines = []
    with (evidence / f"{label}.log").open("w") as log:
        with subprocess.Popen(args, cwd=cwd, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, bufsize=1) as process:
            for line in process.stdout:
                lines.append(line)
                log.write(line)
                log.flush()
                # Cargo's artifacts remain in the evidence for exact binary
                # selection; progress and runtime failures stream immediately.
                if not line.startswith('{"reason":"compiler-artifact"'):
                    print(line, end="", flush=True)
            status = process.wait()
    print(f"PARAGRAPH_DIRECTION_PHASE_END phase={label} status={status} "
          f"elapsed={time.monotonic() - started:.3f}s", flush=True)
    return subprocess.CompletedProcess(command, status, "".join(lines))


def compile_binary(evidence, label):
    # Match the existing shell-full compilation ceiling, independently of the
    # unchanged 3600 s outer control bound. Runtime never invokes Cargo.
    result = run(evidence, f"{label}-compile", 840,
                 COMMAND + ["--no-run", "--message-format=json-render-diagnostics"])
    assert result.returncode == 0, f"{label} compilation failed, not a test verdict"
    rows = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{')]
    binaries = [row["executable"] for row in rows if row.get("reason") == "compiler-artifact"
                and row.get("executable") and row.get("profile", {}).get("test")
                and row.get("target", {}).get("name") == "okilum"]
    assert len(binaries) == 1, f"expected one shell test artifact: {binaries}"
    return str(Path(binaries[0]).resolve())


def controls(evidence):
    candidate_bytes = STATE.read_bytes()
    candidate = candidate_bytes.decode()
    (evidence / "candidate-state.rs").write_bytes(candidate_bytes)
    (evidence / "source.json").write_text(json.dumps({"sha256": digest(candidate_bytes)}))
    method = candidate.index("fn paragraph_rtl_at(")
    begin = candidate.index("        let mut source_bytes = 0;", method)
    end = candidate.index("        rtl\n", begin) + len("        rtl\n")
    # Restore the original full-row materialization/classifier call. Only the
    # same observational diagnostics are added; no product fallback is shipped.
    old = '''        let text = self.text.slice_line(line).to_string();
        self.record_layout_diagnostic(|d| {
            d.paragraph_direction_queries += 1;
            d.paragraph_direction_source_bytes += text.len() as u64;
        });
        crate::input::bidi_geometry::paragraph_is_rtl(&text)
'''
    try:
        STATE.write_text(candidate[:begin] + old + candidate[end:])
        binary = compile_binary(evidence, "old")
        result = run(evidence, "old-test", 120,
                     [binary, TEST, "--exact", "--test-threads=1"], cwd="crates/okilum-shell")
        assert result.returncode not in (124, 137, 143), "old control timed out or was killed"
        assert result.returncode != 0, "old full-row copy unexpectedly passed"
        assert "running 1 test" in result.stdout, "negative control never ran"
        assert f"test {TEST} ..." in result.stdout, "wrong negative control"
        assert "paragraph direction copied long rows:" in result.stdout, result.stdout
        assert "test result: FAILED. 0 passed; 1 failed" in result.stdout, result.stdout
        print("PARAGRAPH_DIRECTION_OLD_CONTROL_FAILED_AS_EXPECTED", flush=True)
    finally:
        restore(evidence)
    binary = compile_binary(evidence, "candidate")
    result = run(evidence, "candidate-test", 120,
                 [binary, "paragraph_direction", "--test-threads=1"], cwd="crates/okilum-shell")
    assert result.returncode == 0, "candidate direction controls failed"
    for name in (TEST, "vendor_bidi_geometry::tests::paragraph_direction_iterator_preserves_first_strong_semantics",
                 "vendor_bidi_geometry::tests::paragraph_direction_follows_the_first_strong_character"):
        assert f"test {name} ... ok" in result.stdout, f"missing positive control: {name}"
    assert STATE.read_bytes() == candidate_bytes, "control left the vendor tree changed"
    print("PARAGRAPH_DIRECTION_CANDIDATE_PASS", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--restore-only", action="store_true")
    args = parser.parse_args()
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    if args.restore_only:
        restore(evidence)
    else:
        controls(evidence)


if __name__ == "__main__":
    main()
