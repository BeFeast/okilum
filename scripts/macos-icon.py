#!/usr/bin/env python3
"""Encode build-only RGBA rasters as a native macOS icon, without new dependencies."""
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import zlib


def png_chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def png(rgba, pixels):
    assert len(rgba) == pixels * pixels * 4
    stride = pixels * 4
    rows = b''.join(b'\0' + rgba[start:start + stride] for start in range(0, len(rgba), stride))
    return (b'\x89PNG\r\n\x1a\n'
            + png_chunk(b'IHDR', struct.pack('>IIBBBBB', pixels, pixels, 8, 6, 0, 0, 0))
            + png_chunk(b'IDAT', zlib.compress(rows, 9)) + png_chunk(b'IEND', b''))


def main():
    raw, output = map(Path, sys.argv[1:])
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='okilum-icon-') as temporary:
        iconset = Path(temporary) / 'Okilum.iconset'
        iconset.mkdir()
        for points in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = points * scale
                suffix = '@2x' if scale == 2 else ''
                name = f'icon_{points}x{points}{suffix}.png'
                (iconset / name).write_bytes(png((raw / f'{pixels}.rgba').read_bytes(), pixels))
        subprocess.run(['/usr/bin/iconutil', '-c', 'icns', str(iconset), '-o', str(output)], check=True)
        container = output.read_bytes()
        assert container[:4] == b'icns' and struct.unpack('>I', container[4:8])[0] == len(container)
        # Exercise the native container reader and verify every encoded representation.
        decoded = Path(temporary) / 'Decoded.iconset'
        subprocess.run(['/usr/bin/iconutil', '-c', 'iconset', str(output), '-o', str(decoded)], check=True)
        dimensions = {}
        for original in sorted(iconset.glob('*.png')):
            data = (decoded / original.name).read_bytes()
            assert data[:8] == b'\x89PNG\r\n\x1a\n' and data[12:16] == b'IHDR'
            actual = struct.unpack('>II', data[16:24])
            expected = struct.unpack('>II', original.read_bytes()[16:24])
            assert actual == expected, original.name
            dimensions[original.name] = list(actual)
        print(json.dumps({'icon': str(output), 'native_roundtrip_dimensions': dimensions}))


if __name__ == '__main__':
    main()
