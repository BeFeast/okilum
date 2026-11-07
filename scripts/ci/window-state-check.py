#!/usr/bin/env python3
"""Compile the old Mac toggle handoff, require runtime failure, restore and pass."""
import pathlib
import subprocess

SOURCE = pathlib.Path("crates/okilum-shell/src/window_state_macos.rs")
QA = pathlib.Path("qa/window-state-2708")
TEST = "window_state::macos::tests::window_state_2708_legacy_zoom_retains_full_geometry_without_double_zoom"
COMMAND = ["cargo", "test", "--locked", "-p", "okilum-shell", "--bin", "okilum"]


def run(label, command):
    result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (QA / (label + ".log")).write_text(result.stdout)
    print(result.stdout, flush=True)
    return result


def main():
    QA.mkdir(parents=True, exist_ok=True)
    committed = SOURCE.read_bytes()
    handoff = b"WindowBounds::Windowed(initial)"
    assert committed.count(handoff) == 1
    try:
        SOURCE.write_bytes(committed.replace(handoff, b"WindowBounds::Maximized(initial)", 1))
        old = run("old-toggle-negative", COMMAND + [TEST, "--", "--exact", "--nocapture", "--test-threads=1"])
        assert old.returncode != 0 and "running 1 test" in old.stdout
        assert "0 passed; 1 failed;" in old.stdout, "require actual old-toggle runtime failure"
    finally:
        SOURCE.write_bytes(committed)
    listed = run("inventory", COMMAND + ["window_state::", "--", "--list"])
    assert listed.returncode == 0
    assert sum("window_state_2708_" in line and line.endswith(": test")
               for line in listed.stdout.splitlines()) >= 6, "new controls must fire"
    restored = run("restored", COMMAND + ["window_state::", "--", "--nocapture", "--test-threads=1"])
    assert restored.returncode == 0 and "0 failed;" in restored.stdout
    assert SOURCE.read_bytes() == committed


if __name__ == "__main__":
    main()
