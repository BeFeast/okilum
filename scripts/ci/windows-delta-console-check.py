#!/usr/bin/env python3
"""Exercise real SDK delta process creation, with an unhidden negative control."""
import os
import json
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
                   "--test", "windows_delta_console", "--no-run", "--message-format=json"]

        def build():
            compiled = subprocess.run(command, cwd=ROOT, env=env, text=True,
                                      stdout=subprocess.PIPE, check=True)
            binaries = [message["executable"] for line in compiled.stdout.splitlines()
                        if (message := json.loads(line)).get("reason") == "compiler-artifact"
                        and message.get("executable") and message["profile"]["test"]]
            if len(binaries) != 1:
                raise RuntimeError(f"Expected one real test executable: {binaries}")
            return [binaries[0], TEST, "--exact", "--nocapture"]

        def run(arguments):
            return subprocess.run(arguments, cwd=ROOT, env=env, text=True,
                                  creationflags=subprocess.CREATE_NEW_CONSOLE,
                                  stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)

        executable = build()
        listing = run(executable + ["--list"])
        listing.check_returncode()
        if f"{TEST}: test" not in listing.stdout.splitlines():
            raise RuntimeError("Windows console regression was not discovered")
        fixed = run(executable)
        print(fixed.stdout, flush=True)
        fixed.check_returncode()

        source = ROOT / "vendor/velopack-1.2.161/src/manager.rs"
        original = source.read_bytes()
        flag = b"patcher.creation_flags(0x0800_0000);"
        if original.count(flag) != 1:
            raise RuntimeError("Pinned delta patcher flag is missing or ambiguous")
        try:
            source.write_bytes(original.replace(flag, b"patcher.creation_flags(0);"))
            baseline = run(build())
            print(baseline.stdout, flush=True)
            if baseline.returncode != 101 or "delta patcher must not have a console" not in baseline.stdout:
                raise RuntimeError("Unhidden SDK did not fail the actual child-console assertion")
        finally:
            source.write_bytes(original)
        subprocess.run(["python", "scripts/velopack-setup.py", "--verify"], cwd=ROOT, check=True)
        # Leave the native target cache proved against the restored production SDK.
        restored = run(build())
        print(restored.stdout, flush=True)
        restored.check_returncode()
    print("SDK delta path: hidden child PASS; unhidden child FAIL as required", flush=True)


if __name__ == "__main__":
    main()
