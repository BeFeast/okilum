#!/usr/bin/env python3
"""Compare real link-dense preview/apply/Undo on one host, before and after #2682."""
import json
import pathlib
import statistics
import subprocess

BASELINE = "2fb8510f9bf45412f0ab0c91082318ec30836e86"
SOURCE = pathlib.Path("crates/okilum-core/src/link_rewrite/mod.rs")
TEST = "folder_move_2682_link_dense_preview_and_exact_undo"
QA = pathlib.Path("qa/folder-move-2682")


def measure(label):
    result = subprocess.run(
        ["cargo", "test", "--locked", "-p", "okilum-core", "--test", "folder_move_performance",
         TEST, "--", "--ignored", "--exact", "--nocapture", "--test-threads=1"],
        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
    )
    (QA / f"{label}.log").write_text(result.stdout)
    print(result.stdout, flush=True)
    assert result.returncode == 0, f"{label}: actual preview/apply/Undo test failed"
    samples = [json.loads(line.split("FOLDER_MOVE_2682_SAMPLE ", 1)[1])
               for line in result.stdout.splitlines() if "FOLDER_MOVE_2682_SAMPLE " in line]
    assert len(samples) == 2 and {s["operation"] for s in samples} == {"rename", "move"}
    assert all(len(s["preview_ms"]) == 3 and s["links"] == 11080 for s in samples)
    assert "1 passed; 0 failed" in result.stdout, "missing nonempty runtime verdict"
    return {s["operation"]: statistics.median(s["preview_ms"]) for s in samples}


def containment_control(committed):
    before = b'let root = crate::vault::canonical_root(&vault.root)?;'
    assert committed.count(before) == 1
    command = ["cargo", "test", "--locked", "-p", "okilum-core", "--lib",
               "link_rewrite::tests::alias_ancestor_attachment_containment_keeps_outside_symlinks_refused",
               "--", "--exact", "--nocapture", "--test-threads=1"]
    try:
        SOURCE.write_bytes(committed.replace(before, b'let root = vault.root.clone();', 1))
        result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        (QA / "containment-negative.log").write_text(result.stdout)
        print(result.stdout, flush=True)
        assert result.returncode != 0 and "running 1 test" in result.stdout
        assert "0 passed; 1 failed;" in result.stdout, "require the old guard's actual runtime failure"
    finally:
        SOURCE.write_bytes(committed)
    result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (QA / "containment-restored.log").write_text(result.stdout)
    print(result.stdout, flush=True)
    assert result.returncode == 0 and "1 passed; 0 failed;" in result.stdout


def main():
    QA.mkdir(parents=True, exist_ok=True)
    committed = SOURCE.read_bytes()
    baseline = subprocess.check_output(["git", "show", f"{BASELINE}:{SOURCE}"])
    try:
        SOURCE.write_bytes(baseline)
        before = measure("baseline")
    finally:
        SOURCE.write_bytes(committed)
    after = measure("fixed")
    report = {"baseline_source": BASELINE, "before_ms": before, "after_ms": after,
              "speedup": {k: before[k] / after[k] for k in before}}
    (QA / "comparison.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2), flush=True)
    assert all(v >= 3 for v in report["speedup"].values()), "expected >=3x preview improvement on both operations"
    containment_control(committed)
    assert SOURCE.read_bytes() == committed, "exact checked source was not restored"


if __name__ == "__main__":
    main()
