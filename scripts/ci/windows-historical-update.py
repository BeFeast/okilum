#!/usr/bin/env python3
"""Download pinned public beta history, verify it, and apply real Full/Delta on Windows."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[2]
TEST = "published_full_and_real_delta_are_applied_on_windows"


def main():
    if os.name != "nt":
        raise RuntimeError("Historical updater application requires native Windows")
    feed = json.loads((ROOT / "scripts/ci/fixtures/windows-historical-update.json").read_text())
    with tempfile.TemporaryDirectory(prefix="okilum-historical-packages-") as temporary:
        destination = Path(temporary)
        for asset in feed["Assets"]:
            path = destination / asset["FileName"]
            request = urllib.request.Request(
                "https://updates.befeast.com/okilum/windows/beta/" + asset["FileName"],
                headers={"User-Agent": "Velopack/1.2.161"})
            sha = hashlib.sha256()
            with urllib.request.urlopen(request, timeout=120) as source, path.open("wb") as output:
                while chunk := source.read(1 << 20):
                    sha.update(chunk)
                    output.write(chunk)
            if path.stat().st_size != asset["Size"] or sha.hexdigest().upper() != asset["SHA256"]:
                raise RuntimeError("Historical package checksum mismatch: " + path.name)
            with zipfile.ZipFile(path) as archive:
                entries = archive.infolist()
                print(json.dumps({"file": path.name, "sha256": sha.hexdigest(),
                                  "entries": len(entries), "largest_payload": max(e.file_size for e in entries),
                                  "zip64": any(e.file_size > 0xffffffff or e.compress_size > 0xffffffff or e.extract_version >= 45 for e in entries)}), flush=True)
        env = dict(os.environ, OKILUM_HISTORICAL_PACKAGES=str(destination))
        command = ["cargo", "test", "--locked", "-p", "okilum-http", "--features", "updates",
                   "--test", "windows_historical_update", TEST, "--"]
        listing = subprocess.run(command + ["--ignored", "--list"], cwd=ROOT, env=env,
                                 text=True, stdout=subprocess.PIPE, check=True).stdout
        if f"{TEST}: test" not in listing.splitlines():
            raise RuntimeError("Real historical Windows control was not discovered")
        subprocess.run(command + ["--ignored", "--exact", "--nocapture"], cwd=ROOT, env=env,
                       check=True, timeout=600)


if __name__ == "__main__":
    main()
