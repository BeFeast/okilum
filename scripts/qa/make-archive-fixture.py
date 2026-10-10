#!/usr/bin/env python3
"""Build the #996 acceptance archive for native QA of the Reader's ZIP view.

    python3 scripts/qa/make-archive-fixture.py OUT.zip

Contents (all synthetic): nested folders; a Markdown note, a PNG and a two-page PDF
(deflated); a log and a CSV; a name stored as CP437 without the UTF-8 flag
("café.txt" written as caf\\x82.txt); and an entry flagged as encrypted (ZipCrypto bit),
which the Reader must list as locked and refuse to open or extract.
Deterministic: fixed timestamps, so the archive hash is stable.
"""
import struct
import sys
import zlib

STAMP = (0x5949, 0x6000)  # DOS date 2024-10-09, time 12:00


def png(width=48, height=32):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    rows = b''.join(b'\x00' + bytes([56, 189, 248]) * width for _ in range(height))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def pdf():
    objects = [
        b'<< /Type /Catalog /Pages 2 0 R >>',
        b'<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>',
        b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R >>',
        None,
        b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 6 0 R >>',
        None,
    ]
    pages = [b'0.22 0.74 0.97 rg 20 20 260 160 re f', b'0.06 0.14 0.27 rg 20 20 260 160 re f']
    objects[3] = b'<< /Length %d >>\nstream\n%s\nendstream' % (len(pages[0]), pages[0])
    objects[5] = b'<< /Length %d >>\nstream\n%s\nendstream' % (len(pages[1]), pages[1])
    out = b'%PDF-1.4\n'
    offsets = []
    for number, body in enumerate(objects, 1):
        offsets.append(len(out))
        out += b'%d 0 obj\n%s\nendobj\n' % (number, body)
    xref = len(out)
    out += b'xref\n0 %d\n0000000000 65535 f \n' % (len(objects) + 1)
    out += b''.join(b'%010d 00000 n \n' % o for o in offsets)
    out += b'trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n' % (len(objects) + 1, xref)
    return out


ENTRIES = [
    # (raw name, data or None for a folder, utf8 flag, encrypted flag, deflate)
    (b'notes/', None, True, False, False),
    (b'notes/projects/', None, True, False, False),
    (b'notes/readme.md', b'# Archive fixture\n\nOpen **entries** in place.\n\n- [[projects/plan]]\n', True, False, True),
    (b'notes/projects/plan.md', b'# Plan\n\n1. Browse\n2. Preview\n3. Extract\n', True, False, True),
    (b'media/', None, True, False, False),
    (b'media/swatch.png', png(), True, False, True),
    (b'media/two-pages.pdf', pdf(), True, False, True),
    (b'data/events.csv', b'day,event,count\n1,open,3\n2,extract,1\n', True, False, True),
    (b'data/run.log', b''.join(b'2024-10-09 12:%02d:00 INFO step %d\n' % (i % 60, i) for i in range(200)), True, False, True),
    ('notes/привет.md'.encode(), b'# UTF-8 name\n', True, False, False),
    (b'caf\x82.txt', b'CP437 name without the UTF-8 flag\n', False, False, False),
    (b'secret/locked.md', b'not really ciphertext, flagged as encrypted', True, True, False),
]


def build():
    out, central = bytearray(), bytearray()
    for name, data, utf8, encrypted, deflate in ENTRIES:
        data = data or b''
        packed = zlib.compress(data, 9)[2:-4] if deflate else data
        method = 8 if deflate else 0
        flags = (1 << 11 if utf8 else 0) | (1 if encrypted else 0)
        crc = zlib.crc32(data)
        fields = struct.pack('<HHHHHIII', 20, flags, method, STAMP[1], STAMP[0], crc, len(packed), len(data))
        offset = len(out)
        out += struct.pack('<I', 0x04034B50) + fields + struct.pack('<HH', len(name), 0) + name + packed
        external = 0o40755 << 16 | 0x10 if data == b'' and name.endswith(b'/') else 0o100644 << 16
        central += (struct.pack('<IH', 0x02014B50, 0x031E) + fields
                    + struct.pack('<HHHHHII', len(name), 0, 0, 0, 0, external, offset) + name)
    start = len(out)
    out += central
    out += struct.pack('<IHHHHIIH', 0x06054B50, 0, 0, len(ENTRIES), len(ENTRIES), len(central), start, 0)
    return bytes(out)


if __name__ == '__main__':
    with open(sys.argv[1], 'wb') as handle:
        handle.write(build())
