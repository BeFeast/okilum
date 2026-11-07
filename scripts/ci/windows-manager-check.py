#!/usr/bin/env python3
"""Non-empty native Windows Manager acceptance and old-cfg negative control."""
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
REGRESSION = "project::tests::windows_manager_1788_uses_native_constructor"


def run(package, selected, *args):
    command = ["cargo", "test", "--locked", "-p", package]
    if package == "okilum-shell":
        command += ["--bin", "okilum"]
    return subprocess.run([*command, selected, "--", *args], cwd=ROOT,
                          text=True, capture_output=True, timeout=1800)


def require_run(package, selected):
    listing = run(package, selected, "--list")
    print(listing.stdout, listing.stderr, flush=True)
    if listing.returncode or not re.search(r": test$", listing.stdout, re.M):
        raise RuntimeError(f"Native Manager tests did not compile/discover: {selected}")
    result = run(package, selected, "--nocapture", "--test-threads=1")
    print(result.stdout, result.stderr, flush=True)
    if result.returncode:
        raise RuntimeError(f"Native Manager tests failed: {selected}")


def main():
    if sys.platform != "win32":
        raise RuntimeError("Native Windows required")
    for package, selected in (
        ("okilum-shell", REGRESSION),
        ("okilum-shell", "project::native::tests::"),
        ("okilum-shell", "project::native::actions::question_progress_tests::"),
        ("okilum-shell", "project::native::onboarding::tests::"),
        ("okilum-shell", "project::credentials::tests::"),
        ("okilum-shell", "reader_t3_settings::manager::tests::"),
        ("okilum-shell", "project::threads::tests::"),
        ("okilum-t3", "stream::tests::"),
    ):
        require_run(package, selected)
    # Build the real old fallback with the regression still present. Require a
    # compiled assertion failure, then restore exact bytes and prove it passes.
    path = ROOT / "crates/okilum-shell/build.rs"
    original = path.read_bytes()
    fixed = b'Ok("linux" | "macos" | "windows")'
    if original.count(fixed) != 1:
        raise RuntimeError("Windows native cfg control no longer matches")
    try:
        path.write_bytes(original.replace(fixed, b'Ok("linux" | "macos")'))
        result = run("okilum-shell", REGRESSION, "--exact", "--nocapture")
        print(result.stdout, result.stderr, flush=True)
        if (result.returncode == 0 or
                "Windows must construct the shared native Manager" not in result.stdout + result.stderr or
                "test result: FAILED. 0 passed; 1 failed;" not in result.stdout):
            raise RuntimeError("Old Windows fallback must compile and fail the constructor assertion")
        print("WINDOWS_MANAGER_1788_NEGATIVE_PASS", flush=True)
    finally:
        path.write_bytes(original)
    require_run("okilum-shell", REGRESSION)
    print("WINDOWS_MANAGER_1788_RESTORED_PASS", flush=True)


if __name__ == "__main__":
    main()
