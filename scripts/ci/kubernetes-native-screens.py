#!/usr/bin/env python3
"""Native screenshots of the Kubernetes Workspace (#3012).

Runs the real `okilum` binary built with the non-publishing
`kubernetes-ui-harness` feature once per composition (Workspace, Triage,
Service path) and theme (light, dark). The harness uses synthetic in-process
data and no kubectl; it writes a ready file when the window shows its
representative state, then this script captures the screen. The first run
also writes an evidence bundle through the real export path, whose hashes are
checked here byte for byte.

Usage: kubernetes-native-screens.py <binary> <output-dir>
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time

COMBOS = [(lens, mode) for lens in ('workspace', 'triage', 'service-path') for mode in ('light', 'dark')]


def capture(path):
    system = platform.system()
    if system == 'Darwin':
        subprocess.run(['screencapture', '-x', '-t', 'png', str(path)], check=True, timeout=60)
    elif system == 'Windows':
        script = (
            'Add-Type -AssemblyName System.Windows.Forms,System.Drawing;'
            '$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds;'
            '$i=New-Object System.Drawing.Bitmap($b.Width,$b.Height);'
            '$g=[System.Drawing.Graphics]::FromImage($i);'
            '$g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size);'
            f'$i.Save("{path}",[System.Drawing.Imaging.ImageFormat]::Png)'
        )
        subprocess.run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script], check=True, timeout=60)
    else:
        subprocess.run(['import', '-window', 'root', str(path)], check=True, timeout=60)


def check_image(path):
    """The capture shows something: a real file with more than one colour."""
    from PIL import Image
    with Image.open(path) as image:
        extrema = image.convert('L').getextrema()
        size = image.size
    assert extrema[0] != extrema[1], f'{path} is a single colour {extrema}'
    return {'size': size, 'luma_range': extrema}


def check_bundle(directory):
    bundles = [p for p in Path(directory).iterdir() if p.is_dir()]
    assert len(bundles) == 1, bundles
    bundle = bundles[0]
    manifest = json.loads((bundle / 'manifest.json').read_text(encoding='utf-8'))
    assert manifest['format'] == 'okilum.kubernetes-evidence/1', manifest['format']
    assert manifest['read_only'] is True
    for line in (bundle / 'SHA256SUMS').read_text(encoding='utf-8').splitlines():
        digest, name = line.split('  ', 1)
        actual = hashlib.sha256((bundle / name).read_bytes()).hexdigest()
        assert actual == digest, (name, actual, digest)
    log = manifest['log_rows']
    assert log and log['file'], log
    assert manifest['log_session']['target'] == log['from_session']['target'], manifest['log_session']
    log_bytes = (bundle / log['file']).read_bytes()
    assert len(log_bytes) == log['bytes'] and log_bytes.endswith(b'(OOMKilled)\n'), log
    everything = b''.join(p.read_bytes() for p in bundle.rglob('*') if p.is_file())
    for forbidden in (b'Bearer', b'BEGIN CERTIFICATE', b'client-key-data', b'token:'):
        assert forbidden not in everything, forbidden
    return {
        'bundle': bundle.name,
        'files': sorted(str(p.relative_to(bundle)) for p in bundle.rglob('*') if p.is_file()),
        'selection': manifest['selection'],
        'log_target': log['from_session']['target'],
        'sources': [(s['kind'], s['state']) for s in manifest['sources']],
    }


def run(binary, lens, mode, out, export):
    with tempfile.TemporaryDirectory(prefix='k8s-ui-', dir=os.environ.get('RUNNER_TEMP')) as tmp:
        ready = Path(tmp) / 'ready'
        env = dict(os.environ, OKILUM_KUBERNETES_HARNESS=f'{lens}:{mode}', OKILUM_KUBERNETES_HARNESS_READY=str(ready))
        if export:
            env['OKILUM_KUBERNETES_HARNESS_EXPORT'] = str(export)
        log_path = out / f'{lens}-{mode}.log'
        with log_path.open('w') as log:
            process = subprocess.Popen([binary], env=env, stdout=log, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 180
                while not ready.exists():
                    assert process.poll() is None, f'{lens}:{mode} exited {process.returncode}: ' + log_path.read_text(encoding='utf-8', errors='replace')[-4000:]
                    assert time.monotonic() < deadline, f'{lens}:{mode} timed out: ' + log_path.read_text(encoding='utf-8', errors='replace')[-4000:]
                    time.sleep(.2)
                state = ready.read_text(encoding='utf-8').strip()
                assert state == 'READY', f'{lens}:{mode} harness reported {state}'
                time.sleep(1.0)
                image = out / f'{lens}-{mode}.png'
                capture(image)
                return {'lens': lens, 'mode': mode, 'image': image.name, **check_image(image)}
            finally:
                process.terminate()
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=20)


def main():
    binary = str(Path(sys.argv[1]).resolve())
    out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    receipt = {'platform': platform.platform(), 'source': os.environ.get('OKILUM_SOURCE_COMMIT'), 'shots': []}
    with tempfile.TemporaryDirectory(prefix='k8s-export-', dir=os.environ.get('RUNNER_TEMP')) as export:
        for index, (lens, mode) in enumerate(COMBOS):
            shot = run(binary, lens, mode, out, Path(export) if index == 0 else None)
            print(json.dumps(shot), flush=True)
            receipt['shots'].append(shot)
        receipt['export'] = check_bundle(export)
    (out / 'receipt.json').write_text(json.dumps(receipt, indent=2), encoding='utf-8')
    print(json.dumps(receipt['export'], indent=2), flush=True)


if __name__ == '__main__':
    main()
