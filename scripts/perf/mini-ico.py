#!/usr/bin/env python3
"""A valid 32x32, 32-bit .ico of one colour: the icon resource of a measurement build.

    mini-ico.py OUT.ico

The Windows build embeds an icon through llvm-rc, which rejects a truncated file. The
production script renders the brand SVG with CairoSVG; a measurement build does not need the
brand icon, so this needs nothing but the standard library.
"""
import struct
import sys

SIZE = 32
pixels = b"".join(struct.pack("<BBBB", 0x2B, 0x6C, 0xD6, 0xFF) for _ in range(SIZE * SIZE))  # BGRA
and_mask = b"\x00" * (4 * SIZE)  # one bit per pixel, rows padded to 4 bytes
dib = struct.pack("<IiiHHIIiiII", 40, SIZE, SIZE * 2, 1, 32, 0, len(pixels) + len(and_mask), 0, 0, 0, 0)
image = dib + pixels + and_mask
entry = struct.pack("<BBBBHHII", SIZE, SIZE, 0, 0, 1, 32, len(image), 6 + 16)
open(sys.argv[1], "wb").write(struct.pack("<HHH", 0, 1, 1) + entry + image)
