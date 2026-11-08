#!/usr/bin/env python3
"""Observe real native table responsiveness, clipboard and unchanged source."""
import hashlib
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import time


def main():
    assert sys.platform in ('darwin', 'win32')
    clipboard = runpy.run_path(str(Path(__file__).with_name('data-view-native-app.py')))['clipboard']
    binary = str(Path(sys.argv[1]).resolve())
    with tempfile.TemporaryDirectory(prefix='data-table-2735-', dir=os.environ['RUNNER_TEMP']) as tmp:
        directory = Path(tmp)
        payload = 'x' * 400
        with (directory / 'large.csv').open('w', encoding='utf-8', newline='') as output:
            output.write('name,n,payload\n')
            for _ in range(120_000):
                output.write(f'needle,2,{payload}\n')
        with (directory / 'large.json').open('w', encoding='utf-8', newline='') as output:
            output.write('[')
            row = json.dumps({'name': 'needle', 'n': 2, 'payload': payload})
            for i in range(120_000):
                output.write((',' if i else '') + row)
            output.write(']')
        with (directory / 'wide.csv').open('w', encoding='utf-8', newline='') as output:
            output.write(','.join(['name', 'n'] + [f'column{i}' for i in range(2, 200)]) + '\n')
            for _ in range(6000):
                output.write(','.join(['needle', '2'] + ['x'] * 198) + '\n')
        def hashes():
            return {name: hashlib.sha256((directory / name).read_bytes()).hexdigest()
                    for name in ('large.csv', 'large.json', 'wide.csv')}
        before = hashes()
        log_path = Path(os.environ['RUNNER_TEMP']) / 'data-table-native-app.log'
        with log_path.open('w', encoding='utf-8') as log:
            process = subprocess.Popen([binary], env=dict(os.environ, OKILUM_TABLE_NATIVE_FIXTURE=str(directory)), stdout=log, stderr=subprocess.STDOUT)
            try:
                for name in before:
                    deadline = time.monotonic() + 120
                    while time.monotonic() < deadline:
                        assert process.poll() is None, log_path.read_text(encoding='utf-8')[-6000:]
                        ready = directory / ('ready-' + name)
                        if ready.exists():
                            assert clipboard() == 'needle', (name, clipboard())
                            receipt = json.loads(ready.read_text(encoding='utf-8'))
                            assert receipt['real_native_platform'] and receipt['ticks'] >= 6
                            assert receipt['max_event_loop_gap_ms'] < 2000
                            print(json.dumps(receipt), flush=True)
                            (directory / ('ack-' + name)).write_text('observed')
                            break
                        time.sleep(.1)
                    else:
                        raise AssertionError('native table timed out: ' + name)
                assert process.wait(timeout=20) == 0
                assert (directory / 'complete').exists()
                assert hashes() == before
                print('DATA_TABLE_2735_REAL_NATIVE_EVENT_LOOP_CLIPBOARD_SOURCE_PASS', flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)


if __name__ == '__main__':
    main()
