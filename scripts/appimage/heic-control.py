#!/usr/bin/env python3
"""Exercise real packaged HEVC decoding, pixels, transforms and corrupt input."""
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parent))
import heif_boxes  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / 'crates/okilum-core/tests/fixtures/viewers/images'
LIMITS = ['--as=1073741824', '--fsize=268435456', '--cpu=20', '--core=0', '--']


def png_pixels(path):
    """Read the tiny RGB/RGBA8 controls without adding a packaging dependency."""
    data = path.read_bytes()
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('Decoder did not produce PNG')
    offset, compressed = 8, bytearray()
    while offset < len(data):
        size, kind = struct.unpack_from('>I4s', data, offset)
        payload = data[offset + 8:offset + 8 + size]
        if kind == b'IHDR':
            width, height, depth, color, compression, filtering, interlace = struct.unpack('>IIBBBBB', payload)
            if (width * height > 4096 or depth != 8 or color not in (2, 6)
                    or compression or filtering or interlace):
                raise ValueError('Unexpected control PNG geometry or encoding')
        if kind == b'IDAT':
            compressed.extend(payload)
        offset += size + 12
    channels = 3 if color == 2 else 4
    stride = width * channels
    raw = zlib.decompress(compressed)
    if len(raw) != (stride + 1) * height:
        raise ValueError('Unexpected control PNG payload length')
    rows, previous = [], bytearray(stride)
    for y in range(height):
        start = y * (stride + 1)
        mode, row = raw[start], bytearray(raw[start + 1:start + 1 + stride])
        for x in range(stride):
            left = row[x - channels] if x >= channels else 0
            up = previous[x]
            corner = previous[x - channels] if x >= channels else 0
            if mode == 0:
                predictor = 0
            elif mode == 1:
                predictor = left
            elif mode == 2:
                predictor = up
            elif mode == 3:
                predictor = (left + up) // 2
            elif mode == 4:
                p = left + up - corner
                predictor = min((left, up, corner), key=lambda v: abs(p - v))
            else:
                raise ValueError('Invalid PNG row filter')
            row[x] = (row[x] + predictor) & 255
        rows.append([tuple(row[x:x + channels]) for x in range(0, stride, channels)])
        previous = row
    return rows


def verify(appdir):
    appdir = appdir.resolve()
    tools = appdir / 'usr/bin'
    # Exercise the executable's real relative loader paths. An audit-only
    # LD_LIBRARY_PATH must not mask missing RPATHs in the shipped artifact.
    # The Ubuntu/Fedora smoke containers intentionally install no HEIF codecs.
    environment = dict(os.environ)
    environment.pop('LD_LIBRARY_PATH', None)
    with tempfile.TemporaryDirectory() as temporary:
        work = Path(temporary)
        outputs = {}
        for name in ('still.heic', 'rotated.heic', 'mirrored.heic', 'multiple.heic',
                     'primary-second.heic'):
            source = (FIXTURES / name).read_bytes()
            # The viewer hides non-primary items before decoding (#2177).
            source = heif_boxes.primary_only(source)
            path = work / "- café 'photo'.HEIF"
            path.write_bytes(source)
            output = work / 'preview.png'
            output.unlink(missing_ok=True)
            (work / 'preview-1.png').unlink(missing_ok=True)
            result = subprocess.run([str(tools / 'prlimit'), *LIMITS,
                                     str(tools / 'heif-convert'), str(path), str(output)],
                                    cwd=work, env=environment, capture_output=True, timeout=30)
            if result.returncode:
                raise ValueError(f'Packaged HEIC decoder failed: {result.stderr.decode(errors="replace")}')
            if path.read_bytes() != source:
                raise ValueError('Decoder changed the original HEIC')
            if not output.exists() or list(work.glob('preview-*.png')):
                raise ValueError(f'{name}: decoder wrote more than the primary image')
            outputs[name] = png_pixels(output)
        base = outputs['still.heic']
        if outputs['multiple.heic'] != base:
            raise ValueError('Multi-image HEIC did not select the primary (first) still')
        second = outputs['primary-second.heic']
        for actual, expected in [(second[40][8][:3], (240, 20, 20)),
                                 (second[8][56][:3], (20, 240, 20))]:
            if max(abs(a - b) for a, b in zip(actual, expected)) > 2:
                raise ValueError(f'Multi-image HEIC did not select the primary (second) still: {actual}')
        if len(base) != 48 or len(base[0]) != 64:
            raise ValueError('Wrong decoded HEVC dimensions')
        for actual, expected in [(base[8][8][:3], (240, 20, 20)),
                                 (base[40][56][:3], (20, 240, 20))]:
            if max(abs(a - b) for a, b in zip(actual, expected)) > 2:
                raise ValueError(f'Wrong decoded HEVC reference pixel: {actual}')
        rotated = [[base[x][63 - y] for x in range(48)] for y in range(64)]
        if outputs['rotated.heic'] != rotated or outputs['mirrored.heic'] != base[::-1]:
            raise ValueError('HEIF rotation/mirror differs from pixel reference')
        for preview in work.glob('preview*.png'):
            preview.unlink()
        path.write_bytes(b'corrupt HEIC control')
        output.unlink(missing_ok=True)
        result = subprocess.run([str(tools / 'prlimit'), *LIMITS,
                                 str(tools / 'heif-convert'), str(path), str(output)],
                                cwd=work, env=environment, capture_output=True, timeout=30)
        if not result.returncode or list(work.glob('preview*.png')):
            raise ValueError('Corrupt HEIC must fail without a stale preview')
    print('HEIC_PACKAGED_PASS real HEVC pixels, irot, imir, primary selection, source preservation, corrupt input')


if __name__ == '__main__':
    verify(Path(sys.argv[1]))
