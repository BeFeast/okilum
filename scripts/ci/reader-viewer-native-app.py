#!/usr/bin/env python3
"""Mount every viewer in a real Reader on the native debug main stack."""
import hashlib
import os
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import struct
import zipfile
import zlib


def assert_restored_source_scope(output):
    restored = re.findall(r'READER_NATIVE file=(\S+) mode=read stage=leave-restored-edit', output)
    assert restored == ['note.md'], f'unexpected restored-edit normalization: {restored!r}'


def assert_file_edit_stages(output):
    stages = re.findall(r'FILE_EDIT_NATIVE stage=(\d+) mounted=Reader', output)
    assert stages == [str(i) for i in range(6)], f'file Edit native flow incomplete: {stages!r}'


def pdf():
    objects = [b'<< /Type /Catalog /Pages 2 0 R >>',
               b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
               b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>',
               b'<< /Length 0 >>\nstream\n\nendstream']
    data = b'%PDF-1.4\n'
    offsets = [0]
    for number, body in enumerate(objects, 1):
        offsets.append(len(data))
        data += f'{number} 0 obj\n'.encode() + body + b'\nendobj\n'
    xref = len(data)
    data += b'xref\n0 5\n0000000000 65535 f \n'
    data += b''.join(f'{offset:010} 00000 n \n'.encode() for offset in offsets[1:])
    return data + f'trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode()


def main():
    binary = str(Path(sys.argv[1]).resolve())
    log_path = Path(os.environ['RUNNER_TEMP']) / 'reader-viewer-native-app.log'
    with tempfile.TemporaryDirectory(prefix='reader-viewer-2795-', dir=os.environ['RUNNER_TEMP']) as tmp:
        directory = Path(tmp)
        vault = directory / 'vault'
        vault.mkdir()
        text = {'data.json': '{"service":"gateway","rows":[1,2,3]}\n',
                'array.json': '[{"id":"first","amount":2100},{"id":"second","amount":4870}]\n',
                'data.csv': 'name,value\nfirst,1\nsecond,2\n',
                'gateway.log': '2026-11-10T12:00:00Z INFO ready\n',
                'scene.canvas': '{"nodes":[{"id":"first","type":"text","x":0,"y":0,"width":320,"height":200,"text":"Reader fixture"}],"edges":[]}',
                'drawing.excalidraw': '{"type":"excalidraw","version":2,"source":"fixture","elements":[{"id":"first","type":"rectangle","x":0,"y":0,"width":100,"height":100,"strokeColor":"transparent","backgroundColor":"#0969e8","fillStyle":"solid","seed":42,"roughness":0}],"appState":{},"files":{}}',
                'source.rs': 'fn main() { println!("Reader fixture"); }\n',
                'note.md': '# Reader fixture\n\nA synthetic note.\n'}
        for name, value in text.items():
            (vault / name).write_bytes(value.encode())
        (vault / 'document.pdf').write_bytes(pdf())
        def chunk(tag, payload):
            return struct.pack('>I', len(payload)) + tag + payload + struct.pack('>I', zlib.crc32(tag + payload) & 0xffffffff)
        (vault / 'image.png').write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\xff')) + chunk(b'IEND', b''))
        with zipfile.ZipFile(vault / 'archive.zip', 'w') as archive:
            archive.writestr('package.json', '{"name":"reader-fixture"}')
        names = list(text) + ['document.pdf', 'image.png', 'archive.zip']
        before = {n: hashlib.sha256((vault / n).read_bytes()).hexdigest() for n in names}
        with log_path.open('w') as log:
            debugger = os.environ.get('DIAG_2825_CDB')
            command = [debugger, '-g', '-G', '-lines', '-y', str(Path(binary).parent), '-c', '.kframes 0x4000; sxd av; sxd eh; sxd clr; sxd cpr; sxd epr; sxd ld; sxe -c "r rsp; kf; !teb; .echo DIAG_2825_SOV_END; q" sov; g', binary] if debugger else [binary]
            process = subprocess.Popen(command, env=dict(os.environ, OKILUM_READER_NATIVE_FIXTURE=str(directory)), stdout=log, stderr=subprocess.STDOUT)
            try:
                assert process.wait(timeout=240) == 0, log_path.read_text(encoding='utf-8')[-10000:]
                assert (directory / 'complete').exists(), 'Reader harness never completed'
                output = log_path.read_text(encoding='utf-8')
                assert_restored_source_scope(output)
                assert_file_edit_stages(output)
                for name in names:
                    assert f'file={name} mode=read mounted=Reader' in output, name
                for name in ['data.json', 'array.json', 'data.csv', 'source.rs', 'note.md']:
                    assert f'file={name} mode=edit mounted=Reader' in output, name
                assert before == {n: hashlib.sha256((vault / n).read_bytes()).hexdigest() for n in names}
                print(output, flush=True)
                print('READER_VIEWER_2795_REAL_NATIVE_MAIN_STACK_PASS', flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)


if __name__ == '__main__':
    main()
