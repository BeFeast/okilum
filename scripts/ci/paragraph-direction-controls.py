#!/usr/bin/env python3
"""Prove the old full-row copy fails the real editor's direction-work bound."""
from pathlib import Path
import subprocess


TEST = ("reader_editor::code_operations::row_layout_tests::"
        "edit2803_paragraph_direction_does_not_copy_reused_long_note_rows")
COMMAND = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin", "okilum"]


def run(arguments):
    result = subprocess.run(COMMAND + arguments, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, check=False)
    print(result.stdout, end="", flush=True)
    return result


def main():
    state = Path("vendor/gpui-component/crates/base/src/input/base/state.rs")
    candidate = state.read_text()
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
        state.write_text(candidate[:begin] + old + candidate[end:])
        result = run([TEST, "--", "--exact", "--test-threads=1"])
        assert result.returncode != 0, "old full-row copy unexpectedly passed"
        assert "running 1 test" in result.stdout, "negative control never ran"
        assert f"test {TEST} ..." in result.stdout, "wrong negative control"
        assert "paragraph direction copied long rows:" in result.stdout, result.stdout
        assert "test result: FAILED. 0 passed; 1 failed" in result.stdout, result.stdout
        print("PARAGRAPH_DIRECTION_OLD_CONTROL_FAILED_AS_EXPECTED", flush=True)
    finally:
        state.write_text(candidate)
    result = run(["paragraph_direction", "--", "--test-threads=1"])
    assert result.returncode == 0, "candidate direction controls failed"
    for name in (TEST, "vendor_bidi_geometry::tests::paragraph_direction_iterator_preserves_first_strong_semantics",
                 "vendor_bidi_geometry::tests::paragraph_direction_follows_the_first_strong_character"):
        assert f"test {name} ... ok" in result.stdout, f"missing positive control: {name}"
    assert state.read_text() == candidate, "control left the vendor tree changed"
    print("PARAGRAPH_DIRECTION_CANDIDATE_PASS", flush=True)


if __name__ == "__main__":
    main()
