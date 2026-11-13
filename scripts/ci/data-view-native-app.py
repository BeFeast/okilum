#!/usr/bin/env python3
"""Real OS viewport/event-loop/clipboard controls for the read-only data viewer."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import struct
import sys
import tempfile
import time


def clipboard():
    args = ['pbpaste'] if sys.platform == 'darwin' else [
        'powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
        '[Console]::OutputEncoding = [Text.UTF8Encoding]::new(); Get-Clipboard -Raw']
    return subprocess.check_output(args, text=True, encoding="utf-8", timeout=10).rstrip('\r\n')


def main():
    assert sys.platform in ('darwin', 'win32')
    binary = str(Path(sys.argv[1]).resolve())
    if sys.platform == 'win32':
        # Record the linked reserve; fixing a render frame must not be confused
        # with silently increasing the native process stack.
        with open(binary, 'rb') as executable:
            executable.seek(0x3c)
            pe_offset = struct.unpack('<I', executable.read(4))[0]
            executable.seek(pe_offset)
            assert executable.read(4) == b'PE\0\0'
            executable.seek(pe_offset + 24)
            assert struct.unpack('<H', executable.read(2))[0] == 0x20b
            executable.seek(pe_offset + 24 + 72)
            stack_reserve = struct.unpack('<Q', executable.read(8))[0]
        print(json.dumps({'native_windows_stack_reserve_bytes': stack_reserve}), flush=True)
    with tempfile.TemporaryDirectory(prefix='data-view-2669-', dir=os.environ['RUNNER_TEMP']) as tmp:
        directory = Path(tmp)
        fixtures = {
            'data.xml': '<root>\n  <child>\n    <value>שלום</value>\n    <other>two</other>\n  </child>\n</root>\n',
            'big.json': '{"long":"' + 'x' * (32 * 1024 * 1024) + '","last":"NEEDLE"}',
            'big.xml': '<root>' + ('<item key="work">value0123456789</item>' * (32 * 1024 * 1024 // 37)) + '<item key="last">NEEDLE</item></root>',
            'big.txt': ('abcdefgh' * 31 + '\n') * (32 * 1024 * 1024 // 249),
        }
        for name, text in fixtures.items():
            (directory / name).write_bytes(text.encode())
        before = {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in fixtures}
        log_path = Path(os.environ['RUNNER_TEMP']) / 'data-view-native-app.log'
        with log_path.open('w') as log:
            process = subprocess.Popen([binary], env=dict(os.environ, OKILUM_DATA_VIEW_NATIVE_FIXTURE=str(directory)), stdout=log, stderr=subprocess.STDOUT)
            sampler = None
            if sys.platform == 'darwin' and os.environ.get('DIAG_2825_SAMPLE'):
                # Diagnostic only: 1 ms main-thread call tree across data.xml open.
                sampler = subprocess.Popen(['sample', str(process.pid), '7', '1', '-mayDie', '-fullPaths', '-file', os.environ['DIAG_2825_SAMPLE']], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
            try:
                for name, expected in [('data.xml', '<value>שלום</value>'), ('big.json', '"last":"NEEDLE"'), ('big.xml', '<item key="last">NEEDLE</item>'), ('big.txt', 'abcdefgh')]:
                    deadline = time.monotonic() + 120
                    while time.monotonic() < deadline:
                        assert process.poll() is None, f"native app exited {process.returncode} before {name}: " + log_path.read_text(encoding="utf-8")[-6000:]
                        ready = directory / ('ready-' + name)
                        if ready.exists():
                            assert clipboard() == expected, (name, clipboard())
                            receipt = json.loads(ready.read_text(encoding="utf-8"))
                            assert receipt['real_native_platform'] and receipt['ticks'] >= 6
                            assert receipt['initial_view'] == 'raw', receipt
                            assert receipt['final_view'] == ('tree' if name in ('big.json', 'big.xml') else 'raw'), receipt
                            print(json.dumps(receipt), flush=True)
                            (directory / ('ack-' + name)).write_text('observed')
                            break
                        time.sleep(.1)
                    else:
                        raise AssertionError('native viewer timeout: ' + name)
                if sampler is not None:
                    sampler.wait(timeout=60)
                assert process.wait(timeout=20) == 0, log_path.read_text(encoding="utf-8")[-6000:]
                assert (directory / 'complete').exists()
                trace = log_path.read_text(encoding='utf-8')
                assert 'DATA_VIEW_2669_NATIVE_TRACE chrome.begin' in trace
                assert 'DATA_VIEW_2669_NATIVE_TRACE chrome.end' in trace
                assert 'DATA_VIEW_2669_NATIVE_TRACE toolbar.begin' not in trace
                assert before == {name: hashlib.sha256((directory / name).read_bytes()).hexdigest() for name in fixtures}
                print('DATA_VIEW_2669_REAL_NATIVE_EVENT_LOOP_CLIPBOARD_SOURCE_PASS', flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)


if __name__ == '__main__':
    main()
