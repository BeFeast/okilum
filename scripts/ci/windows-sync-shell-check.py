#!/usr/bin/env python3
"""Compile the shipping Windows Settings/preview composition on its native SDK."""
import os
from pathlib import Path
import struct
import subprocess
import tempfile


def check():
    if os.name != "nt":
        raise RuntimeError("Windows shell composition requires the native Windows SDK")
    with tempfile.TemporaryDirectory(prefix="okilum-sync-shell-") as temporary:
        icon = Path(temporary) / "compile-only.ico"
        # A disposable resource for cargo check only. Release packaging continues
        # to rasterize the approved brand SVG with windows-icon.py.
        size = 16
        dib = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0,
                          size * size * 4, 0, 0, 0, 0)
        dib += bytes([0, 0, 0, 255]) * size * size + bytes(4 * size)
        icon.write_bytes(struct.pack("<HHH", 0, 1, 1)
                         + struct.pack("<BBBBHHII", size, size, 0, 0, 1, 32, len(dib), 22) + dib)
        env = dict(os.environ, OKILUM_WINDOWS_ICON=str(icon), CARGO_PROFILE_DEV_DEBUG="0")
        subprocess.run(["cargo", "check", "--locked", "-p", "okilum-shell", "--bin", "okilum",
                        "--no-default-features"], env=env, check=True)


if __name__ == "__main__":
    check()
