#!/usr/bin/env python3
"""Exercise real SDK delta process creation, with an unhidden negative control."""
import os
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]
TEST = "windows_delta_preparation_has_no_console_and_keeps_the_delta_path"


def main():
    with tempfile.TemporaryDirectory(prefix="okilum-delta-console-") as temporary:
        helper = Path(temporary) / "Update.exe"
        subprocess.run(["rustc", "--edition=2021", "scripts/ci/windows-delta-console-helper.rs",
                        "-o", str(helper)], cwd=ROOT, check=True)
        env = dict(os.environ, OKILUM_DELTA_CONSOLE_HELPER=str(helper))
        command = ["cargo", "test", "--locked", "-p", "okilum-http", "--features", "updates",
                   "--test", "windows_delta_console", TEST, "--"]
        listing = subprocess.run(command + ["--list", "--exact"], cwd=ROOT,
                                 env=env, text=True, stdout=subprocess.PIPE, check=True)
        if f"{TEST}: test" not in listing.stdout.splitlines():
            raise RuntimeError("Windows console regression was not discovered")
        subprocess.run(command + ["--exact", "--nocapture"], cwd=ROOT, env=env, check=True)

        source = ROOT / "vendor/velopack-1.2.161/src/manager.rs"
        original = source.read_bytes()
        flag = b"patcher.creation_flags(0x0800_0000);"
        if original.count(flag) != 1:
            raise RuntimeError("Pinned delta patcher flag is missing or ambiguous")
        try:
            source.write_bytes(original.replace(flag, b"patcher.creation_flags(0);"))
            baseline = subprocess.run(command + ["--exact", "--nocapture"], cwd=ROOT,
                                      env=env, text=True, stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT, check=False)
            print(baseline.stdout, flush=True)
            if baseline.returncode != 101 or "delta patcher must not have a console" not in baseline.stdout:
                raise RuntimeError("Unhidden SDK did not fail the actual child-console assertion")
        finally:
            source.write_bytes(original)
        subprocess.run(["python", "scripts/velopack-setup.py", "--verify"], cwd=ROOT, check=True)
        # Leave the native target cache proved against the restored production SDK.
        subprocess.run(command + ["--exact", "--nocapture"], cwd=ROOT, env=env, check=True)
    print("SDK delta path: hidden child PASS; unhidden child FAIL as required", flush=True)


if __name__ == "__main__":
    main()
