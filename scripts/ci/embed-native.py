#!/usr/bin/env python3
"""Observe production embed actions using the real native clipboard/file manager.

The app is built alone with embed-native-harness, without GPUI test-support.
All files are synthetic, isolated under RUNNER_TEMP. No live user vault is used.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def native(script):
    args = (["osascript", "-e", script] if sys.platform == "darwin" else
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
             "[Console]::OutputEncoding = [Text.UTF8Encoding]::new(); " + script])
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=10).stdout.strip()


def clipboard():
    if sys.platform == "darwin":
        return subprocess.run(["pbpaste"], check=True, capture_output=True,
                              text=True, timeout=10).stdout
    return native("Get-Clipboard -Raw")


def selection():
    if sys.platform == "darwin":
        return native('tell application "Finder" to get POSIX path of (item 1 of (get selection) as alias)')
    return native("$s = New-Object -ComObject Shell.Application; "
                  "$s.Windows() | ForEach-Object { $_.Document.SelectedItems() | "
                  "ForEach-Object { $_.Path } }")


def wait_for(probe, description):
    end = time.monotonic() + 25
    error = None
    while time.monotonic() < end:
        try:
            if probe():
                return
        except subprocess.SubprocessError as exc:
            error = str(exc)
        time.sleep(.2)
    raise AssertionError(f"native {description} was not observed: {error}")


def main():
    assert sys.platform in ("darwin", "win32"), "Mac or Windows platform required"
    binary = str(Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix="embed-1622-", dir=os.environ["RUNNER_TEMP"]) as tmp:
        directory = Path(tmp)
        root = directory / "vault"
        (root / "attachments").mkdir(parents=True)
        relative = "attachments/manual space.pdf"
        target = root / relative
        payload = b"%PDF-1.0 synthetic acceptance file\n"
        target.write_bytes(payload)
        source = f"# Files\n\n![[{relative}]]\n"
        (root / "note.md").write_bytes(source.encode("utf-8"))
        # Seed a value that cannot be mistaken for either copied path.
        if sys.platform == "darwin":
            subprocess.run(["pbcopy"], input=b"native sentinel", check=True)
        else:
            native("Set-Clipboard -Value 'native sentinel'")
        assert clipboard().strip() == "native sentinel", "clipboard positive control"
        env = dict(os.environ, OKILUM_EMBED_NATIVE_FIXTURE=str(directory))
        with (Path(os.environ["RUNNER_TEMP"]) / "embed-native-app.log").open("w") as log:
            process = subprocess.Popen([binary], env=env, stdout=log, stderr=subprocess.STDOUT)
            try:
                for phase, expected in [("relative", relative), ("absolute", str(target.resolve())),
                                        ("reveal", str(target.resolve()))]:
                    def ready():
                        if process.poll() is not None:
                            details = (Path(os.environ["RUNNER_TEMP"]) / "embed-native-app.log").read_text()
                            raise AssertionError(f"native app exited before {phase}: {details[-4000:]}")
                        return (directory / f"ready-{phase}").exists()
                    wait_for(ready, phase + " dispatch")
                    if phase == "reveal":
                        def revealed():
                            return any(os.path.normcase(os.path.normpath(item)) ==
                                       os.path.normcase(expected) for item in selection().splitlines())
                        wait_for(revealed, "Finder/Explorer exact selected file")
                    else:
                        wait_for(lambda: clipboard() == expected, phase + " system clipboard")
                    (directory / f"ack-{phase}").write_text("observed", encoding="utf-8")
                    print(json.dumps({"phase": phase, "native_observed": True}), flush=True)
                wait_for(lambda: (directory / "complete").exists(), "completion")
                assert process.wait(timeout=15) == 0
                assert (root / "note.md").read_bytes() == source.encode()
                assert target.read_bytes() == payload
                print("EMBED_NATIVE_CLIPBOARD_REVEAL_PASS", flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)


if __name__ == "__main__":
    main()
