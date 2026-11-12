#!/usr/bin/env python3
"""Reader-hosted Tree budget: 3.5 MB, 414 paths, native CPU draw <=16 ms."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def fixture():
    doc = {
        'components': {'schemas': {f'Type{i:04}': {'type': 'object', 'description': 'Public fixture ' + 'x' * 2100} for i in range(1214)},
                       'securitySchemes': {'bearerAuth': {'type': 'http', 'scheme': 'bearer'}}},
        'paths': {f'/v1/path-{i:04}': {'get': {'description': 'Public route ' + 'x' * 1800, 'responses': {'200': {'description': 'OK'}}}} for i in range(414)},
        'security': [{'bearerAuth': []}], '_padding': ''}
    text = json.dumps(doc, separators=(',', ':'))
    doc['_padding'] = 'x' * (3_500_000 - len(text))
    text = json.dumps(doc, separators=(',', ':'))
    assert len(text) == 3_500_000
    return text


def main():
    binary = str(Path(sys.argv[1]).resolve())
    parent = Path(os.environ['RUNNER_TEMP'])
    with tempfile.TemporaryDirectory(prefix='code-toolbar-2810-', dir=parent) as temp:
        root = Path(temp)
        (root / 'vault').mkdir()
        path = root / 'vault/api.json'
        path.write_text(fixture(), encoding='utf-8')
        before = hashlib.sha256(path.read_bytes()).hexdigest()
        log_path = parent / 'code-toolbar-native-app.log'
        with log_path.open('w') as log:
            process = subprocess.run([binary], env=dict(os.environ, OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE=str(root)),
                                     stdout=log, stderr=subprocess.STDOUT, timeout=180)
        assert process.returncode == 0, log_path.read_text(encoding='utf-8')[-14000:]
        receipt = json.loads((root / 'toolbar-receipt.json').read_text())
        assert receipt['real_native_platform'] and receipt['mounted'] == 'Reader'
        assert receipt['bytes'] == 3_500_000 and receipt['paths_keys'] == 414
        assert receipt['open_ms'] <= 100
        for marked in [False, True]:
            frames = [f for f in receipt['frames'] if f['marks'] == marked]
            assert len(frames) == 48
            assert max(f['cpu_draw_ms'] for f in frames) <= 16
            assert all(f['rows_built'] > 0 for f in frames)
            assert len({f['scroll_y'] for f in frames}) == 48
        assert hashlib.sha256(path.read_bytes()).hexdigest() == before
        receipt['fixture_sha256'] = before
        (parent / 'code-toolbar-native-receipt.json').write_text(json.dumps(receipt, indent=2))
        print(json.dumps(receipt), flush=True)
        print('CODE2810_NATIVE_READER_BUDGET_PASS', flush=True)


if __name__ == '__main__':
    main()
