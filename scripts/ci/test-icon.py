#!/usr/bin/env python3
"""Write a minimal valid 16x16 .ico (classic 32-bit DIB entry) for test-only Windows builds.

The shell's build script embeds an icon from OKILUM_WINDOWS_ICON and refuses to build without
one. Release builds get the approved icon from scripts/windows-icon.py (which needs cairo); a
lane that only runs tests needs the resource to exist and be well formed, not to look right.
Never packaged, never shipped: the file lives in a temporary directory.

usage: python3 scripts/ci/test-icon.py OUTPUT.ico
"""
import struct
import sys
from pathlib import Path

SIZE = 16
# One opaque pixel colour (BGRA), bottom-up rows as a DIB stores them.
pixels = b"\x30\x30\x30\xff" * (SIZE * SIZE)
mask = b"\x00" * (4 * SIZE)  # 1 bit per pixel, rows padded to 4 bytes; all zero = opaque
header = struct.pack("<IiiHHIIiiII", 40, SIZE, SIZE * 2, 1, 32, 0, len(pixels) + len(mask), 0, 0, 0, 0)
image = header + pixels + mask
directory = struct.pack("<HHH", 0, 1, 1) + struct.pack("<BBBBHHII", SIZE, SIZE, 0, 0, 1, 32, len(image), 6 + 16)
output = Path(sys.argv[1])
output.parent.mkdir(parents=True, exist_ok=True)
output.write_bytes(directory + image)
print(f"wrote {output} ({output.stat().st_size} bytes)")
