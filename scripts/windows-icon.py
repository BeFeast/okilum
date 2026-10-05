#!/usr/bin/env python3
"""Rasterize the approved SVG into a build-only Windows icon."""
from pathlib import Path
import struct
import sys
import cairosvg

source, output = map(Path, sys.argv[1:])
images = [(size, cairosvg.svg2png(url=str(source), output_width=size, output_height=size))
          for size in (16, 32, 48, 64, 128, 256)]
offset = 6 + 16 * len(images)
entries = []
for size, data in images:
    entries.append(struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32,
                               len(data), offset))
    offset += len(data)
output.parent.mkdir(parents=True, exist_ok=True)
output.write_bytes(struct.pack('<HHH', 0, 1, len(images)) + b''.join(entries)
                   + b''.join(data for _, data in images))
